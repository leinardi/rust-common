/*
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Licensed under either of
 *
 *  * Apache License, Version 2.0 (LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0)
 *  * MIT license (LICENSE-MIT or https://opensource.org/licenses/MIT)
 *
 * at your option.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! The client over TCP and from the environment (`tcp`), as swarm-scheduler-exporter's Go client
//! tests drive it: `DOCKER_HOST`, `DOCKER_API_VERSION`, the TCP request, gzip, the
//! response-header timeout, a reset connection, and moby's wording of a TCP failure.
//!
//! Extracted from swarm-scheduler-exporter/src/docker/tests.rs at c9e79afa8810.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::BodyExt as _;
use socket2::SockRef;
use tokio::net::TcpListener;

use super::{
    EXPORTER_USER_AGENT, FakeDaemon, Outcome, Recorded, Reply, Script, TempDir, engine,
    find_source, list_containers, ok, respond, serve, two_calls,
};
use crate::version::validate_client_api_version;
use crate::{Client, DEFAULT_DOCKER_HOST, ErrorKind, Settings};

/// The environment the client reads, from pairs; anything else is unset.
pub(super) fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    move |name| vars.get(name).cloned()
}

pub(super) fn settings(vars: &[(&str, &str)]) -> Settings {
    Settings::from_env(env(vars)).unwrap()
}

/// swarm-scheduler-exporter's client: its `User-Agent` and a 30s response-header timeout.
fn exporter(settings: Settings) -> Client {
    Client::new(
        settings
            .user_agent(EXPORTER_USER_AGENT.to_owned())
            .unwrap()
            .response_header_timeout(Some(Duration::from_secs(30))),
    )
}

#[cfg(feature = "tls")]
pub(super) type Acceptor = Option<tokio_rustls::TlsAcceptor>;
#[cfg(not(feature = "tls"))]
pub(super) type Acceptor = Option<std::convert::Infallible>;

impl FakeDaemon {
    /// A daemon on a fresh loopback TCP port.
    pub(super) fn tcp(script: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Self {
        Self::tcp_with(None, Arc::new(script))
    }

    /// A daemon on a fresh loopback TCP port, behind TLS with an acceptor.
    pub(super) fn tcp_with(acceptor: Acceptor, script: Script) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(listener).unwrap();
        let host = format!("tcp://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            let connections = AtomicUsize::new(0);
            while let Ok((stream, _)) = listener.accept().await {
                let id = connections.fetch_add(1, Ordering::SeqCst);
                let script = Arc::clone(&script);
                let recorded = Arc::clone(&recorded);
                #[cfg(feature = "tls")]
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let reset = SockRef::from(&stream).try_clone().ok();
                    let outcome = match acceptor {
                        #[cfg(feature = "tls")]
                        Some(acceptor) => match acceptor.accept(stream).await {
                            Ok(tls) => serve(tls, id, &script, &recorded).await,
                            Err(_) => return,
                        },
                        #[cfg(not(feature = "tls"))]
                        Some(never) => match never {},
                        None => serve(stream, id, &script, &recorded).await,
                    };
                    if matches!(outcome, Outcome::Reset)
                        && let Some(socket) = reset
                    {
                        let _ = socket.set_linger(Some(Duration::ZERO));
                    }
                });
            }
        });
        Self {
            requests,
            host,
            _dir: None,
        }
    }

    /// A client from the environment with the daemon as `DOCKER_HOST` and `vars`, moby's
    /// defaults otherwise.
    pub(super) fn client_env(&self, vars: &[(&str, &str)]) -> Client {
        let mut all = vec![("DOCKER_HOST", self.host.as_str())];
        all.extend_from_slice(vars);
        Client::new(settings(&all))
    }
}

/// Go: `TestNewDockerClient_DefaultHost`
#[test]
fn default_host() {
    let settings = settings(&[]);
    assert_eq!(settings.daemon_host(), DEFAULT_DOCKER_HOST);
    assert_eq!(settings.socket(), Some("/var/run/docker.sock"));
    assert_eq!(settings.client_version(), "1.56");
}

/// Go: `TestNewDockerClient_UnixSocketFromEnv`, plus the request Go writes over a socket: the
/// `api.moby.localhost` host and no `Accept-Encoding`.
#[tokio::test]
async fn unix_socket_from_env() {
    let daemon = FakeDaemon::unix(engine(Some("1.56")));
    let client = exporter(settings(&[("DOCKER_HOST", &daemon.host)]));
    assert_eq!(client.settings().daemon_host(), daemon.host);
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    let requests = daemon.requests();
    assert_eq!(daemon.wire(), ["HEAD /_ping", "GET /v1.56/containers/json"]);
    assert_eq!(
        requests[1].head,
        format!(
            "GET /v1.56/containers/json HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: \
             {EXPORTER_USER_AGENT}"
        )
    );
    assert_eq!(
        requests[0].head,
        format!(
            "HEAD /_ping HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: \
             {EXPORTER_USER_AGENT}"
        )
    );
    // One connection for both: the ping's was reused.
    assert_eq!(requests[0].connection, requests[1].connection);
}

/// The request Go writes over TCP: `host:port` as the host, Go's header case and order, and the
/// transport's `Accept-Encoding: gzip` on a `GET` (not on the `HEAD`); `Accept` values as
/// separate lines.
#[tokio::test]
async fn tcp_requests_match_go_s_wire() {
    let daemon = FakeDaemon::tcp(engine(Some("1.56")));
    let client = exporter(settings(&[("DOCKER_HOST", &daemon.host)]));
    let response = client
        .get(
            "/events",
            "since=1",
            &["application/jsonl", "application/x-ndjson;q=0.9"],
        )
        .await
        .unwrap();
    drop(response);
    let requests = daemon.requests();
    let authority = daemon.host.trim_start_matches("tcp://");
    assert_eq!(
        requests[0].head,
        format!("HEAD /_ping HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: {EXPORTER_USER_AGENT}")
    );
    assert_eq!(
        requests[1].head,
        format!(
            "GET /v1.56/events?since=1 HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: \
             {EXPORTER_USER_AGENT}\r\nAccept: application/jsonl\r\nAccept: \
             application/x-ndjson;q=0.9\r\nAccept-Encoding: gzip"
        )
    );
    assert_eq!(requests[1].header("accept").len(), 2);
    assert_eq!(client.settings().socket(), None);
}

/// A `tcp://` URL's path prefixes every request, the ping's included.
#[tokio::test]
async fn a_base_path_prefixes_every_request() {
    let daemon = FakeDaemon::tcp(|request| {
        if request.target == "/base/_ping" {
            ok(&[("Api-Version", "1.51")], "OK")
        } else {
            ok(&[], "[]")
        }
    });
    let client = Client::new(settings(&[(
        "DOCKER_HOST",
        &format!("{}/base/", daemon.host),
    )]));
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    assert_eq!(
        daemon.wire(),
        ["HEAD /base/_ping", "GET /base/v1.51/containers/json"]
    );
}

/// Go's transport decompresses a gzip body it asked for, and drops the encoding headers.
#[tokio::test]
async fn tcp_gzip_body_is_decompressed() {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"[{\"ID\":\"n1\"}]").unwrap();
    let gzipped = encoder.finish().unwrap();
    let daemon = FakeDaemon::tcp(move |request| {
        if request.target == "/_ping" {
            return ok(&[("Api-Version", "1.56")], "OK");
        }
        Reply::Respond {
            status: 200,
            headers: vec![("Content-Encoding", "gzip".to_owned())],
            body: gzipped.clone(),
        }
    });
    let client = daemon.client();
    let response = client.get("/nodes", "", &[]).await.unwrap();
    assert!(response.headers().get("content-encoding").is_none());
    assert!(response.headers().get("content-length").is_none());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"[{\"ID\":\"n1\"}]");
}

/// A gzip body that does not decompress fails with `gzip: …`, the `io::Error` kept as the cause.
#[tokio::test]
async fn gzip_body_errors_keep_the_original_io_cause() {
    let daemon = FakeDaemon::tcp(|_| ok(&[("Content-Encoding", "gzip")], "invalid gzip"));
    let client = daemon.client_env(&[("DOCKER_API_VERSION", "1.56")]);
    let error = list_containers(&client).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Body);
    assert!(error.to_string().starts_with("gzip: "));
    assert!(find_source::<std::io::Error>(&error).is_some());
}

/// A reset exchange keeps hyper's error as the cause.
#[tokio::test]
async fn exchange_errors_keep_the_original_hyper_cause() {
    let daemon = FakeDaemon::tcp(|_| Reply::Reset);
    let client = daemon.client_env(&[("DOCKER_API_VERSION", "1.56")]);
    let error = list_containers(&client).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert!(find_source::<hyper::Error>(&error).is_some());
}

/// Go: `TestNewDockerClient_ResponseHeaderTimeoutBoundsTheEventStream`: a daemon that accepts
/// and never answers cannot hold the event stream; the pin skips the ping, so the stream request
/// is the first one sent.
#[tokio::test]
async fn response_header_timeout_bounds_the_event_stream() {
    let daemon = FakeDaemon::tcp(|_| Reply::Silent);
    let settings = settings(&[
        ("DOCKER_HOST", daemon.host.as_str()),
        ("DOCKER_API_VERSION", "1.56"),
    ])
    .response_header_timeout(Some(Duration::from_millis(100)));
    let client = Client::new(settings);
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), client.get("/events", "", &[]))
        .await
        .expect("the stream to a silent daemon was still waiting for headers after 5s");
    let error = result.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert_eq!(
        error.to_string(),
        format!(
            "Cannot connect to the Docker daemon at {}. Is the docker daemon running?",
            daemon.host
        )
    );
    assert_eq!(daemon.wire(), ["GET /v1.56/events"]);
}

/// Go: `TestValidateClientAPIVersion`
#[test]
fn validate_client_api_version_cases() {
    let cases = [
        ("", true),
        ("1.39", false),
        ("1.40", true),
        ("1.56", true),
        ("1.57", false),
        ("v1.44", true),
    ];
    for (pin, valid) in cases {
        let settings = settings(&[("DOCKER_API_VERSION", pin)]);
        let result = validate_client_api_version(settings.client_version());
        assert_eq!(result.is_ok(), valid, "{pin:?}: {result:?}");
        if !valid {
            assert_eq!(
                result.unwrap_err().to_string(),
                format!(
                    "unsupported Docker API version {pin} (DOCKER_API_VERSION): supported range \
                     is 1.40 to 1.56"
                )
            );
        }
    }
}

/// `DOCKER_API_VERSION` is trimmed and loses one `v`; a malformed one is moby's error, under
/// the `docker client:` prefix.
#[test]
fn api_version_from_env_follows_moby() {
    assert_eq!(
        settings(&[("DOCKER_API_VERSION", " v1.45 ")]).client_version(),
        "1.45"
    );
    assert_eq!(
        settings(&[("DOCKER_API_VERSION", "01.050")]).client_version(),
        "1.50"
    );
    assert_eq!(
        settings(&[("DOCKER_API_VERSION", " v ")]).client_version(),
        "1.56"
    );
    for (pin, reason) in [
        ("vv1.45", "must be formatted <major>.<minor>"),
        ("1", "must be formatted <major>.<minor>"),
        (
            "x.1",
            "invalid major version: must be formatted <major>.<minor>",
        ),
        (
            "1.x",
            "invalid minor version: must be formatted <major>.<minor>",
        ),
        (
            "1.45.2",
            "invalid minor version: must be formatted <major>.<minor>",
        ),
    ] {
        assert_eq!(
            Settings::from_env(env(&[("DOCKER_API_VERSION", pin)]))
                .unwrap_err()
                .to_string(),
            format!("docker client: invalid API version ({pin}): {reason}"),
        );
    }
    assert_eq!(
        Settings::from_env(env(&[("DOCKER_HOST", "nope")]))
            .unwrap_err()
            .to_string(),
        "docker client: unable to parse docker host `nope`"
    );
    assert_eq!(
        Settings::from_env(env(&[("DOCKER_HOST", "npipe:////./pipe/docker_engine")]))
            .unwrap_err()
            .to_string(),
        "docker client: protocol not available"
    );
}

/// Without `tls`, a certificate path is refused rather than ignored.
#[cfg(not(feature = "tls"))]
#[test]
fn a_cert_path_without_tls_is_refused() {
    assert_eq!(
        Settings::from_env(env(&[("DOCKER_CERT_PATH", "/certs")]))
            .unwrap_err()
            .to_string(),
        "configure TLS from DOCKER_CERT_PATH=/certs: TLS is not built in (the docker-engine `tls` \
         feature)"
    );
}

/// A pinned version (`DOCKER_API_VERSION=' v1.45 '`) never pings.
#[tokio::test]
async fn pinned_version_never_pings() {
    let daemon = FakeDaemon::tcp(engine(Some("1.51")));
    let client = daemon.client_env(&[("DOCKER_API_VERSION", " v1.45 ")]);
    let (wire, version) = two_calls(&daemon, &client).await;
    assert_eq!(
        wire,
        ["GET /v1.45/containers/json", "GET /v1.45/containers/json"]
    );
    assert_eq!(version, "1.45");
}

/// A connection the daemon closed while idle is replaced without failing the request, as Go's
/// transport retries a `GET` on a reused connection.
#[tokio::test]
async fn idle_connection_closed_by_the_daemon_is_retried() {
    let daemon =
        FakeDaemon::tcp(|request| Reply::RespondThenClose(Box::new(engine(Some("1.56"))(request))));
    let client = daemon.client_env(&[("DOCKER_API_VERSION", "1.56")]);
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    // Let the close reach the client, as it would after an idle timeout.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    let requests = daemon.requests();
    assert_eq!(requests.len(), 2);
    assert_ne!(requests[0].connection, requests[1].connection);
}

/// moby's wording of the failures from the environment: a TCP port nothing listens on, a socket
/// path that does not exist, and the response errors over a socket named by `DOCKER_HOST`.
#[tokio::test]
async fn errors_are_worded_as_moby_from_the_environment() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    let host = format!("tcp://{address}");
    let client = Client::new(settings(&[
        ("DOCKER_HOST", &host),
        ("DOCKER_API_VERSION", "1.56"),
    ]));
    assert_eq!(
        list_containers(&client).await.unwrap_err().to_string(),
        format!("Cannot connect to the Docker daemon at {host}. Is the docker daemon running?")
    );

    let missing = TempDir::new();
    let path = missing.path().join("docker.sock");
    let host = format!("unix://{}", path.display());
    let client = Client::new(settings(&[
        ("DOCKER_HOST", &host),
        ("DOCKER_API_VERSION", "1.56"),
    ]));
    assert_eq!(
        list_containers(&client).await.unwrap_err().to_string(),
        format!(
            "failed to connect to the docker API at {host}; check if the path is correct and if \
             the daemon is running: dial unix {}: connect: no such file or directory",
            path.display()
        )
    );

    let daemon = FakeDaemon::unix(|request| match request.target.as_str() {
        "/v1.56/json" => respond(
            404,
            &[("Content-Type", "application/json")],
            "{\"message\":\" no such service \"}",
        ),
        _ => respond(404, &[], ""),
    });
    let client = daemon.client_env(&[("DOCKER_API_VERSION", "1.56")]);
    let error = client.get("/json", "", &[]).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Error response from daemon: no such service"
    );
    let socket = daemon.socket().replace('/', "%2F");
    assert_eq!(
        client.get("/empty", "", &[]).await.unwrap_err().to_string(),
        format!(
            "request returned 404 Not Found for API route and version \
             http://{socket}/v1.56/empty, check if the server supports the requested API version"
        )
    );
}

/// A response that is not HTTP over plain TCP is moby's hint about a TLS daemon.
#[tokio::test]
async fn a_tls_daemon_reached_without_tls_is_hinted_at() {
    let daemon = FakeDaemon::tcp(|_| Reply::Raw(b"\x16\x03\x01\x00\x02\x02\x28".to_vec()));
    let client = daemon.client_env(&[("DOCKER_API_VERSION", "1.56")]);
    let error = list_containers(&client).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "Get \"http://{}/v1.56/containers/json\": net/http: HTTP/1.x transport connection \
             broken: malformed HTTP response.\n* Are you trying to connect to a TLS-enabled daemon \
             without TLS?",
            daemon.host.trim_start_matches("tcp://")
        )
    );
}
