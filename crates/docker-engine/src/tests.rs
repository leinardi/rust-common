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

//! The client against a fake daemon: ports of the Go client tests, one test per negotiation case
//! of moby's, the wire both programs' Go clients wrote, moby's error wording, the causes kept
//! behind it, the context, and with their features TCP, gzip, TLS, the writes and the attach.
//! A negotiation test runs over every transport the features build (a unix socket, and TCP).
//!
//! Extracted from swarm-scheduler-exporter/src/docker/tests.rs at c9e79afa8810 (TCP, TLS, gzip,
//! the environment, the header timeout, the causes).
//! Extracted from swarm-device-access/src/docker/tests.rs at 8b4ab3a3e417 (the unix socket's
//! wire, the bounded ping drain, the write-safe retry, the context).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::BodyExt as _;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::UnixListener;

use crate::types::null_default;
use crate::{CallContext, Client, ContextError, ErrorKind, Settings, with_context};

#[cfg(feature = "tcp")]
mod tcp;
#[cfg(feature = "tls")]
mod tls;
#[cfg(feature = "write")]
mod write;

/// The `User-Agent` swarm-scheduler-exporter sends, in place of moby's.
const EXPORTER_USER_AGENT: &str = "swarm-scheduler-exporter/1.2.3";

/// The first `T` in `error`'s chain of sources.
fn find_source<T: std::error::Error + 'static>(error: &dyn std::error::Error) -> Option<&T> {
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(cause) = cause.downcast_ref::<T>() {
            return Some(cause);
        }
        source = cause.source();
    }
    None
}

/// A temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "docker-engine-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    #[cfg(feature = "tls")]
    fn str(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A request the fake daemon received.
#[derive(Debug, Clone)]
struct Recorded {
    connection: usize,
    method: String,
    target: String,
    head: String,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Vec<String> {
        self.head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(": "))
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.to_owned())
            .collect()
    }
}

/// What the fake daemon does with a request.
#[derive(Debug, Clone)]
enum Reply {
    /// A response with a `Content-Length` (no body written for `HEAD`).
    Respond {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    },
    /// These bytes as they are, then the connection closed.
    Raw(Vec<u8>),
    /// The connection closed before any response.
    Close,
    /// A TCP reset (`SO_LINGER` 0), before any response; a close on a unix socket.
    Reset,
    /// The connection closed after the response, unannounced, as an idle timeout closes it.
    RespondThenClose(Box<Reply>),
    /// Never an answer.
    Silent,
    /// The head of a `status` answer announcing a body of `length` bytes, which never comes.
    StallBody {
        status: u16,
        headers: Vec<(&'static str, String)>,
        length: usize,
    },
    /// `Reply` after a delay.
    After(Duration, Box<Reply>),
}

fn ok(headers: &[(&'static str, &str)], body: &str) -> Reply {
    respond(200, headers, body)
}

fn respond(status: u16, headers: &[(&'static str, &str)], body: &str) -> Reply {
    Reply::Respond {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect(),
        body: body.as_bytes().to_vec(),
    }
}

type Script = Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>;

/// A daemon answering by script, recording every request.
struct FakeDaemon {
    requests: Arc<Mutex<Vec<Recorded>>>,
    /// `unix://<socket>` or `tcp://<address>`.
    host: String,
    _dir: Option<TempDir>,
}

/// How a connection ended.
enum Outcome {
    Closed,
    Reset,
}

impl FakeDaemon {
    /// A daemon on a fresh unix socket.
    fn unix(script: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Self {
        let dir = TempDir::new();
        let socket = dir.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let script: Script = Arc::new(script);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            let connections = AtomicUsize::new(0);
            loop {
                let id = connections.fetch_add(1, Ordering::SeqCst);
                let script = Arc::clone(&script);
                let recorded = Arc::clone(&recorded);
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    serve(stream, id, &script, &recorded).await;
                });
            }
        });
        Self {
            requests,
            host: format!("unix://{}", socket.display()),
            _dir: Some(dir),
        }
    }

    /// The socket path, for a unix daemon.
    fn socket(&self) -> &str {
        self.host.strip_prefix("unix://").unwrap()
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// `METHOD target` of every request, in order.
    fn wire(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| format!("{} {}", request.method, request.target))
            .collect()
    }

    /// A client with moby's defaults: [`Settings::unix`] for a unix daemon, `DOCKER_HOST` for a
    /// TCP one.
    fn client(&self) -> Client {
        match self.host.strip_prefix("unix://") {
            Some(socket) => Client::new(Settings::unix(socket).unwrap()),
            #[cfg(feature = "tcp")]
            None => self.client_env(&[]),
            #[cfg(not(feature = "tcp"))]
            None => unreachable!("a TCP daemon without the tcp feature"),
        }
    }
}

/// Serves one connection by script until it closes.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    connection: usize,
    script: &Script,
    recorded: &Mutex<Vec<Recorded>>,
) -> Outcome {
    let mut buffer = Vec::new();
    loop {
        let head = loop {
            if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                buffer.drain(..end + 4);
                break head;
            }
            let mut chunk = [0_u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return Outcome::Closed,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            }
        };
        let mut request_line = head.lines().next().unwrap_or_default().split(' ');
        let mut request = Recorded {
            connection,
            method: request_line.next().unwrap_or_default().to_owned(),
            target: request_line.next().unwrap_or_default().to_owned(),
            head: head.clone(),
            body: Vec::new(),
        };
        let length: usize = request
            .header("content-length")
            .first()
            .and_then(|length| length.parse().ok())
            .unwrap_or_default();
        while buffer.len() < length {
            let mut chunk = [0_u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return Outcome::Closed,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            }
        }
        request.body = buffer.drain(..length).collect();
        recorded.lock().unwrap().push(request.clone());
        let mut reply = script(&request);
        let mut close_after = false;
        loop {
            match reply {
                Reply::After(delay, then) => {
                    tokio::time::sleep(delay).await;
                    reply = *then;
                }
                Reply::RespondThenClose(then) => {
                    close_after = true;
                    reply = *then;
                }
                Reply::Respond { .. }
                | Reply::Raw(_)
                | Reply::Close
                | Reply::Reset
                | Reply::Silent
                | Reply::StallBody { .. } => break,
            }
        }
        match reply {
            Reply::Respond {
                status,
                headers,
                body,
            } => {
                let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status));
                for (name, value) in &headers {
                    write!(out, "{name}: {value}\r\n").unwrap();
                }
                write!(out, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
                let mut bytes = out.into_bytes();
                if request.method != "HEAD" {
                    bytes.extend_from_slice(&body);
                }
                if stream.write_all(&bytes).await.is_err() {
                    return Outcome::Closed;
                }
                let _ = stream.flush().await;
                if close_after {
                    let _ = stream.shutdown().await;
                    return Outcome::Closed;
                }
            }
            Reply::Raw(bytes) => {
                let _ = stream.write_all(&bytes).await;
                let _ = stream.shutdown().await;
                return Outcome::Closed;
            }
            Reply::Close => return Outcome::Closed,
            Reply::Reset => return Outcome::Reset,
            Reply::Silent => std::future::pending::<()>().await,
            Reply::StallBody {
                status,
                headers,
                length,
            } => {
                return stall_body(&mut stream, status, &headers, length).await;
            }
            Reply::After(..) | Reply::RespondThenClose(_) => unreachable!(),
        }
    }
}

/// [`Reply::StallBody`]: the head, then nothing, until the client goes away.
async fn stall_body<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    headers: &[(&'static str, String)],
    length: usize,
) -> Outcome {
    let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    for (name, value) in headers {
        write!(out, "{name}: {value}\r\n").unwrap();
    }
    write!(out, "Content-Length: {length}\r\n\r\n").unwrap();
    if stream.write_all(out.as_bytes()).await.is_err() {
        return Outcome::Closed;
    }
    let _ = stream.flush().await;
    std::future::pending().await
}

fn reason(status: u16) -> &'static str {
    hyper::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Unknown")
}

/// A daemon that pings with `Api-Version` (or none) and lists nothing.
fn engine(api_version: Option<&'static str>) -> impl Fn(&Recorded) -> Reply + Clone + Send + Sync {
    move |request| {
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(version) = api_version {
            headers.push(("Api-Version", version));
        }
        if request.target == "/_ping" {
            ok(&headers, "OK")
        } else {
            ok(&headers, "[]")
        }
    }
}

/// The same script on every transport the features build: a unix socket, and TCP.
fn each_transport(
    script: impl Fn(&Recorded) -> Reply + Clone + Send + Sync + 'static,
) -> Vec<FakeDaemon> {
    #[cfg(feature = "tcp")]
    let tcp = FakeDaemon::tcp(script.clone());
    let daemons = vec![FakeDaemon::unix(script)];
    #[cfg(feature = "tcp")]
    let daemons = {
        let mut daemons = daemons;
        daemons.push(tcp);
        daemons
    };
    daemons
}

async fn list_containers(client: &Client) -> Result<String, crate::Error> {
    let response = client.get("/containers/json", "", &[]).await?;
    let body = response.into_body().collect().await?.to_bytes();
    Ok(String::from_utf8(body.to_vec()).unwrap())
}

/// Two container lists, and the wire they produce.
async fn two_calls(daemon: &FakeDaemon, client: &Client) -> (Vec<String>, String) {
    for _ in 0..2 {
        assert_eq!(list_containers(client).await.unwrap(), "[]");
    }
    (daemon.wire(), client.client_version().await)
}

/// The socket a program passes as `unix://<socket>`, and the version before any request.
#[tokio::test]
async fn settings_name_the_socket() {
    let settings = Settings::unix("/var/run/docker.sock").unwrap();
    assert_eq!(settings.daemon_host(), "unix:///var/run/docker.sock");
    assert_eq!(settings.socket(), Some("/var/run/docker.sock"));
    assert_eq!(settings.client_version(), "1.56");
    assert_eq!(settings.user_agent_value(), crate::moby_user_agent());
    assert_eq!(
        Settings::unix("").unwrap_err().to_string(),
        "unable to parse docker host `unix://`"
    );
    let too_long = format!("/{}", "a".repeat(108));
    assert_eq!(
        Settings::unix(&too_long).unwrap_err().to_string(),
        format!("unix socket path \"{too_long}\" is too long")
    );
    assert_eq!(Client::new(settings).client_version().await, "1.56");
}

/// The requests swarm-device-access's Go daemon wrote over its socket (its
/// `tests/testdata/engine_wire`): the `api.moby.localhost` host, moby's `User-Agent`, no
/// `Accept-Encoding`, `Accept` values as separate lines in moby's order, and the ping's
/// connection reused.
#[tokio::test]
async fn requests_match_the_go_wire() {
    let daemon = FakeDaemon::unix(engine(Some("1.56")));
    let client = daemon.client();
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    let response = client
        .get("/events", "since=1", &crate::events::EVENTS_ACCEPT)
        .await
        .unwrap();
    drop(response);
    let requests = daemon.requests();
    assert_eq!(
        daemon.wire(),
        [
            "HEAD /_ping",
            "GET /v1.56/containers/json",
            "GET /v1.56/events?since=1"
        ]
    );
    let user_agent = format!("moby-client/v0.6.0 linux/{}", crate::go_arch());
    assert_eq!(
        requests[0].head,
        format!("HEAD /_ping HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: {user_agent}")
    );
    assert_eq!(
        requests[1].head,
        format!(
            "GET /v1.56/containers/json HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: \
             {user_agent}"
        )
    );
    assert_eq!(
        requests[2].head,
        format!(
            "GET /v1.56/events?since=1 HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: \
             {user_agent}\r\nAccept: application/jsonl\r\nAccept: \
             application/x-ndjson;q=0.9\r\nAccept: application/json-seq;q=0.5"
        )
    );
    assert_eq!(requests[2].header("accept").len(), 3);
    // One connection for the ping and the list: the ping's was reused.
    assert_eq!(requests[0].connection, requests[1].connection);
}

/// swarm-scheduler-exporter's settings over a socket (`TestNewDockerClient_UnixSocketFromEnv`):
/// its own `User-Agent` on every request, the `HEAD` included.
#[tokio::test]
async fn a_caller_s_user_agent_replaces_moby_s() {
    let daemon = FakeDaemon::unix(engine(Some("1.56")));
    let settings = Settings::unix(daemon.socket())
        .unwrap()
        .user_agent(EXPORTER_USER_AGENT.to_owned())
        .unwrap()
        .response_header_timeout(Some(Duration::from_secs(30)));
    let client = Client::new(settings);
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    let requests = daemon.requests();
    assert_eq!(
        requests[0].head,
        format!(
            "HEAD /_ping HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: \
             {EXPORTER_USER_AGENT}"
        )
    );
    assert_eq!(
        requests[1].head,
        format!(
            "GET /v1.56/containers/json HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: \
             {EXPORTER_USER_AGENT}"
        )
    );
    assert_eq!(requests[0].connection, requests[1].connection);
}

/// A `User-Agent` no header may carry is refused when it is set, with net/http's message, never
/// swapped for another one; the settings keep the one they had.
#[test]
fn a_user_agent_no_header_may_carry_is_refused() {
    let settings = Settings::unix("/run/docker.sock").unwrap();
    for invalid in [
        "swarm-scheduler-exporter/1.2.3\r\nX: y",
        "a\0b",
        "del\u{7f}",
    ] {
        let error = settings.clone().user_agent(invalid.to_owned()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "net/http: invalid header field value for \"User-Agent\"",
            "{invalid:?}"
        );
        assert!(Settings::check_user_agent(invalid).is_err(), "{invalid:?}");
    }
    // A tab and text beyond ASCII pass, as Go's `ValidHeaderFieldValue` lets them.
    for valid in ["a\tb", "exporter/1.0 (ünïcode)"] {
        let set = settings.clone().user_agent(valid.to_owned()).unwrap();
        assert_eq!(set.user_agent_value(), valid);
    }
    assert_eq!(settings.user_agent_value(), crate::moby_user_agent());
}

/// Settings compare field by field; a changed `User-Agent` or timeout makes them differ.
#[test]
fn settings_compare_by_value() {
    let settings = Settings::unix("/run/docker.sock").unwrap();
    assert_eq!(settings, Settings::unix("/run/docker.sock").unwrap());
    assert_ne!(settings, Settings::unix("/run/other.sock").unwrap());
    assert_ne!(
        settings,
        settings.clone().user_agent("x/1".to_owned()).unwrap()
    );
    assert_ne!(
        settings,
        settings
            .clone()
            .response_header_timeout(Some(Duration::from_secs(1)))
    );
}

/// moby's `GOARCH` names, which its `User-Agent` carries.
#[test]
fn the_user_agent_names_go_s_architecture() {
    let arch = crate::go_arch();
    assert!(!arch.is_empty());
    if cfg!(target_arch = "x86_64") {
        assert_eq!(arch, "amd64");
    }
    if cfg!(target_arch = "aarch64") {
        assert_eq!(arch, "arm64");
    }
    assert_eq!(
        crate::moby_user_agent(),
        format!("moby-client/v0.6.0 {}/{arch}", std::env::consts::OS)
    );
}

/// moby: a `HEAD` 200 is used as is, with no `GET`, and the version is negotiated once.
#[tokio::test]
async fn ping_head_200_negotiates_down() {
    for daemon in each_transport(engine(Some("1.51"))) {
        let (wire, version) = two_calls(&daemon, &daemon.client()).await;
        assert_eq!(
            wire,
            [
                "HEAD /_ping",
                "GET /v1.51/containers/json",
                "GET /v1.51/containers/json"
            ],
            "{}",
            daemon.host
        );
        assert_eq!(version, "1.51");
    }
}

/// moby: no `Api-Version` means 1.56.
#[tokio::test]
async fn ping_without_api_version_is_1_56() {
    for daemon in each_transport(engine(None)) {
        let (wire, version) = two_calls(&daemon, &daemon.client()).await;
        assert_eq!(
            wire,
            [
                "HEAD /_ping",
                "GET /v1.56/containers/json",
                "GET /v1.56/containers/json"
            ],
            "{}",
            daemon.host
        );
        assert_eq!(version, "1.56");
    }
}

/// moby: 1.39 is below the minimum; moby's error is discarded, the request goes out at 1.56,
/// and the next request pings again.
#[tokio::test]
async fn ping_below_minimum_is_not_refused_and_pings_again() {
    for daemon in each_transport(engine(Some("1.39"))) {
        let (wire, version) = two_calls(&daemon, &daemon.client()).await;
        assert_eq!(
            wire,
            [
                "HEAD /_ping",
                "GET /v1.56/containers/json",
                "HEAD /_ping",
                "GET /v1.56/containers/json"
            ],
            "{}",
            daemon.host
        );
        assert_eq!(version, "1.56");
    }
}

/// moby: a `HEAD` 404 or 405 (swarm-device-access's capture against such an Engine) falls back
/// to `GET /_ping`, whose short body is drained, as moby's `ensureReaderClosed` does, so its
/// connection is reused.
#[tokio::test]
async fn ping_head_404_or_405_falls_back_to_get() {
    for status in [404, 405] {
        let script = move |request: &Recorded| {
            if request.method == "HEAD" {
                respond(status, &[], "")
            } else {
                engine(Some("1.51"))(request)
            }
        };
        for daemon in each_transport(script) {
            let (wire, version) = two_calls(&daemon, &daemon.client()).await;
            assert_eq!(
                wire,
                [
                    "HEAD /_ping",
                    "GET /_ping",
                    "GET /v1.51/containers/json",
                    "GET /v1.51/containers/json"
                ],
                "{status} {}",
                daemon.host
            );
            assert_eq!(version, "1.51");
            let requests = daemon.requests();
            assert_eq!(requests[1].connection, requests[2].connection);
        }
    }
}

/// The control for the drain: a `GET /_ping` body far longer than moby's 512 bytes (longer than
/// one read of hyper's) is not read to its end, so its connection is closed rather than reused.
#[tokio::test]
async fn a_long_ping_body_is_not_drained() {
    let long = "x".repeat(1024 * 1024);
    let daemon = FakeDaemon::unix(move |request| {
        if request.method == "HEAD" {
            respond(405, &[], "")
        } else if request.target == "/_ping" {
            ok(&[("Api-Version", "1.51")], &long)
        } else {
            engine(Some("1.51"))(request)
        }
    });
    let client = daemon.client();
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    let requests = daemon.requests();
    assert_eq!(
        daemon.wire(),
        ["HEAD /_ping", "GET /_ping", "GET /v1.51/containers/json"]
    );
    assert_ne!(requests[1].connection, requests[2].connection);
}

/// A daemon whose `HEAD /_ping` fails and whose `GET /_ping` answers `status` with a body that
/// never comes.
fn stalled_ping_body(status: u16) -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    move |request| {
        if request.method == "HEAD" {
            respond(405, &[], "")
        } else if request.target == "/_ping" {
            Reply::StallBody {
                status,
                headers: vec![("Api-Version", "1.51".to_owned())],
                length: 2,
            }
        } else {
            engine(Some("1.51"))(request)
        }
    }
}

/// With a response-header timeout, a ping body that stalls is dropped with its connection once
/// the timeout passes, a drained 200's as well as a 500's error body: the negotiation, which every
/// request waits on, is held no longer than the headers could hold it, and the request goes out
/// on a new connection (at the version moby falls back to when the 500 fails the ping).
#[tokio::test]
async fn a_stalled_ping_body_is_dropped_after_the_response_header_timeout() {
    for (status, listed_at) in [
        (200, "/v1.51/containers/json"),
        (500, "/v1.56/containers/json"),
    ] {
        let daemon = FakeDaemon::unix(stalled_ping_body(status));
        let client = Client::new(
            Settings::unix(daemon.socket())
                .unwrap()
                .response_header_timeout(Some(Duration::from_secs(1))),
        );
        let listed = tokio::time::timeout(Duration::from_secs(10), list_containers(&client))
            .await
            .unwrap_or_else(|_| panic!("{status}: the stalled body is not bounded"));
        assert_eq!(listed.unwrap(), "[]", "{status}");
        let get = format!("GET {listed_at}");
        assert_eq!(
            daemon.wire(),
            ["HEAD /_ping", "GET /_ping", get.as_str()],
            "{status}"
        );
        let requests = daemon.requests();
        assert_ne!(requests[1].connection, requests[2].connection, "{status}");
    }
}

/// The ping a stalled error body fails: the daemon's status, as a response error, and a text that
/// says what timed out.
#[tokio::test]
async fn a_stalled_ping_error_body_fails_the_ping_with_its_status() {
    let daemon = FakeDaemon::unix(stalled_ping_body(500));
    let client = Client::new(
        Settings::unix(daemon.socket())
            .unwrap()
            .response_header_timeout(Some(Duration::from_secs(1))),
    );
    let error = tokio::time::timeout(Duration::from_secs(10), client.ping())
        .await
        .expect("the stalled error body is not bounded")
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Response);
    assert_eq!(
        error.status(),
        Some(hyper::StatusCode::INTERNAL_SERVER_ERROR)
    );
    let text = error.to_string();
    assert!(
        text.starts_with(
            "request returned 500 Internal Server Error for API route and version http://"
        ) && text.ends_with("/_ping; its body did not arrive within 1s"),
        "{text}"
    );
}

/// The control: without one, moby's default, the drain waits for the body as moby's does, and
/// only the caller's own bound ends the call.
#[tokio::test]
async fn without_a_response_header_timeout_a_stalled_ping_body_holds_the_call() {
    let daemon = FakeDaemon::unix(stalled_ping_body(200));
    let client = daemon.client();
    let listed = tokio::time::timeout(Duration::from_secs(2), list_containers(&client)).await;
    assert!(listed.is_err(), "the drain returned without the body");
    assert_eq!(daemon.wire(), ["HEAD /_ping", "GET /_ping"]);
}

/// moby: a `HEAD` whose connection closes (or, on TCP, is reset) without an answer falls back to
/// `GET /_ping`.
#[tokio::test]
async fn ping_head_closed_falls_back_to_get() {
    for failure in [Reply::Close, Reply::Reset] {
        let script = move |request: &Recorded| {
            if request.method == "HEAD" {
                failure.clone()
            } else {
                engine(Some("1.51"))(request)
            }
        };
        for daemon in each_transport(script) {
            let (wire, version) = two_calls(&daemon, &daemon.client()).await;
            assert_eq!(
                wire,
                [
                    "HEAD /_ping",
                    "GET /_ping",
                    "GET /v1.51/containers/json",
                    "GET /v1.51/containers/json"
                ],
                "{}",
                daemon.host
            );
            assert_eq!(version, "1.51");
        }
    }
}

/// moby: `HEAD` and `GET` both 500; the request still goes out at 1.56 and the next one pings
/// again.
#[tokio::test]
async fn ping_both_failing_still_sends_and_pings_again() {
    let script = |request: &Recorded| {
        if request.target == "/_ping" {
            respond(500, &[("Api-Version", "1.51")], "boom")
        } else {
            engine(Some("1.51"))(request)
        }
    };
    for daemon in each_transport(script) {
        let (wire, version) = two_calls(&daemon, &daemon.client()).await;
        assert_eq!(
            wire,
            [
                "HEAD /_ping",
                "GET /_ping",
                "GET /v1.56/containers/json",
                "HEAD /_ping",
                "GET /_ping",
                "GET /v1.56/containers/json",
            ],
            "{}",
            daemon.host
        );
        assert_eq!(version, "1.56");
    }
}

/// moby: a daemon above the maximum negotiates 1.56, once.
#[tokio::test]
async fn ping_above_maximum_is_1_56() {
    for daemon in each_transport(engine(Some("1.60"))) {
        let (wire, version) = two_calls(&daemon, &daemon.client()).await;
        assert_eq!(
            wire,
            [
                "HEAD /_ping",
                "GET /v1.56/containers/json",
                "GET /v1.56/containers/json"
            ],
            "{}",
            daemon.host
        );
        assert_eq!(version, "1.56");
    }
}

/// The explicit ping swarm-device-access's Go daemon sends before every subscription:
/// unversioned, and no negotiation of its own; a failing `GET` is moby's response error.
#[tokio::test]
async fn the_explicit_ping_does_not_negotiate() {
    let daemon = FakeDaemon::unix(engine(Some("1.51")));
    let client = daemon.client();
    assert_eq!(client.ping().await.unwrap(), "1.51");
    assert_eq!(client.client_version().await, "1.56");
    assert_eq!(daemon.wire(), ["HEAD /_ping"]);

    let failing = FakeDaemon::unix(|_| respond(500, &[("Content-Type", "text/plain")], "down"));
    let error = failing.client().ping().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Response);
    assert_eq!(error.to_string(), "Error response from daemon: down");
    assert_eq!(failing.wire(), ["HEAD /_ping", "GET /_ping"]);
}

/// Concurrent first requests share one ping (moby sends one each), whether it negotiates or
/// fails; after a failed one, the next request pings again.
#[tokio::test]
async fn concurrent_first_requests_share_one_ping() {
    let slow_ping = |version: Option<&'static str>, status: u16| {
        move |request: &Recorded| {
            if request.target == "/_ping" {
                let mut headers = Vec::new();
                if let Some(version) = version {
                    headers.push(("Api-Version", version));
                }
                Reply::After(
                    Duration::from_millis(100),
                    Box::new(respond(status, &headers, "OK")),
                )
            } else {
                ok(&[], "[]")
            }
        }
    };
    for (version, status, pings) in [(Some("1.51"), 200, 1), (None, 500, 2)] {
        for daemon in each_transport(slow_ping(version, status)) {
            let client = Arc::new(daemon.client());
            let mut calls = Vec::new();
            for _ in 0..8 {
                let client = Arc::clone(&client);
                calls.push(tokio::spawn(async move {
                    list_containers(&client).await.unwrap()
                }));
            }
            for call in calls {
                call.await.unwrap();
            }
            let wire = daemon.wire();
            let ping_requests = wire.iter().filter(|line| line.ends_with("/_ping")).count();
            assert_eq!(ping_requests, pings, "{status}: {wire:?}");
            let want = if status == 200 {
                "GET /v1.51/containers/json"
            } else {
                "GET /v1.56/containers/json"
            };
            assert_eq!(
                wire.iter().filter(|line| *line == want).count(),
                8,
                "{wire:?}"
            );
            if status != 200 {
                list_containers(&client).await.unwrap();
                let wire = daemon.wire();
                assert_eq!(
                    wire[wire.len() - 3..],
                    ["HEAD /_ping", "GET /_ping", "GET /v1.56/containers/json"]
                );
            }
        }
    }
}

/// A connection the daemon closed while idle is replaced without failing the request, as Go's
/// transport retries a `GET` on a reused connection.
#[tokio::test]
async fn idle_connection_closed_by_the_daemon_is_replaced() {
    let script =
        |request: &Recorded| Reply::RespondThenClose(Box::new(engine(Some("1.56"))(request)));
    for daemon in each_transport(script) {
        let client = daemon.client();
        assert_eq!(list_containers(&client).await.unwrap(), "[]");
        // Let the close reach the client, as it would after an idle timeout.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(list_containers(&client).await.unwrap(), "[]");
        let requests = daemon.requests();
        assert_eq!(
            daemon.wire(),
            [
                "HEAD /_ping",
                "GET /v1.56/containers/json",
                "GET /v1.56/containers/json"
            ]
        );
        assert_ne!(requests[1].connection, requests[2].connection);
    }
}

/// A daemon that dropped a reused connection after reading a `GET` gets the `GET` again on a new
/// connection: net/http replays the safe methods.
#[tokio::test]
async fn a_get_is_resent_on_a_dropped_connection() {
    let first_list = Arc::new(AtomicUsize::new(0));
    let lists = Arc::clone(&first_list);
    let daemon = FakeDaemon::unix(move |request| {
        if request.target == "/v1.56/containers/json" && lists.fetch_add(1, Ordering::SeqCst) == 0 {
            Reply::Close
        } else {
            engine(Some("1.56"))(request)
        }
    });
    let client = daemon.client();
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    let requests = daemon.requests();
    assert_eq!(
        daemon.wire(),
        [
            "HEAD /_ping",
            "GET /v1.56/containers/json",
            "GET /v1.56/containers/json"
        ]
    );
    assert_eq!(requests[0].connection, requests[1].connection);
    assert_ne!(requests[1].connection, requests[2].connection);
}

/// moby's wording of the failures over a socket, and the status of a response error.
#[tokio::test]
async fn errors_are_worded_as_moby() {
    // A socket file nothing listens on.
    let refused = TempDir::new();
    let path = refused.path().join("docker.sock");
    drop(UnixListener::bind(&path).unwrap());
    let host = format!("unix://{}", path.display());
    let client = Client::new(Settings::unix(&path.display().to_string()).unwrap());
    assert_eq!(
        list_containers(&client).await.unwrap_err().to_string(),
        format!("Cannot connect to the Docker daemon at {host}. Is the docker daemon running?")
    );

    // A socket path that does not exist.
    let missing = TempDir::new();
    let path = missing.path().join("docker.sock");
    let host = format!("unix://{}", path.display());
    let client = Client::new(Settings::unix(&path.display().to_string()).unwrap());
    let error = list_containers(&client).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert_eq!(
        error.to_string(),
        format!(
            "failed to connect to the docker API at {host}; check if the path is correct and if \
             the daemon is running: dial unix {}: connect: no such file or directory",
            path.display()
        )
    );

    // Response errors: a JSON message, a plain body, an empty body, a JSON body without message.
    let daemon = FakeDaemon::unix(|request| match request.target.as_str() {
        "/_ping" => ok(&[("Api-Version", "1.56")], "OK"),
        "/v1.56/json" => respond(
            404,
            &[("Content-Type", "application/json")],
            "{\"message\":\" No such container: c \"}",
        ),
        "/v1.56/plain" => respond(500, &[("Content-Type", "text/plain")], " oops\n"),
        "/v1.56/empty" => respond(404, &[], ""),
        _ => respond(500, &[("Content-Type", "application/json")], "{}"),
    });
    let client = daemon.client();
    let failure = |path: &'static str| {
        let client = &client;
        async move { client.get(path, "", &[]).await.unwrap_err() }
    };
    let error = failure("/json").await;
    assert_eq!(error.kind(), ErrorKind::Response);
    assert_eq!(error.status(), Some(hyper::StatusCode::NOT_FOUND));
    assert!(error.is_not_found());
    assert_eq!(
        error.to_string(),
        "Error response from daemon: No such container: c"
    );
    assert_eq!(
        failure("/plain").await.to_string(),
        "Error response from daemon: oops"
    );
    let socket = daemon.socket().replace('/', "%2F");
    assert_eq!(
        failure("/empty").await.to_string(),
        format!(
            "request returned 404 Not Found for API route and version \
             http://{socket}/v1.56/empty, check if the server supports the requested API version"
        )
    );
    assert_eq!(
        failure("/nomessage").await.to_string(),
        "Error response from daemon: API returned a 500 (Internal Server Error) but provided no \
         error-message"
    );
}

/// A node as a program declares one.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Node {
    #[serde(rename = "ID", deserialize_with = "null_default")]
    id: String,
}

/// The generic calls decode as Go decodes, and the event stream is picked by its type.
#[tokio::test]
async fn lists_inspects_and_events_decode_as_go() {
    let daemon = FakeDaemon::unix(|request| match request.target.as_str() {
        "/_ping" => ok(&[("Api-Version", "1.56")], "OK"),
        "/v1.56/nodes" => ok(&[], r#"[null,{"ID":"n1"}]"#),
        "/v1.56/nodes/n1" => ok(&[], r#"{"ID":"n1"} trailing"#),
        _ => ok(
            &[("Content-Type", "application/json-seq")],
            "\u{1e}{\"ID\":\"e1\"}\n\u{1e}{\"ID\":\"e2\"}\n",
        ),
    });
    let client = daemon.client();
    let nodes: Vec<Node> = client.get_list("/nodes", "").await.unwrap();
    assert_eq!(
        nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        ["", "n1"]
    );
    let node: Node = client.get_object("/nodes/n1").await.unwrap();
    assert_eq!(node.id, "n1");
    let mut events = client.events("since=1").await.unwrap();
    assert!(events.filters_rs());
    assert_eq!(events.next::<Node>().await.unwrap().id, "e1");
    assert_eq!(events.next::<Node>().await.unwrap().id, "e2");
    assert!(events.next::<Node>().await.unwrap_err().is_eof());
    let events = daemon.requests().pop().unwrap();
    assert_eq!(events.target, "/v1.56/events?since=1");
    assert_eq!(events.header("accept").len(), 3);
}

/// A decode error keeps serde's error as its cause, shared by its clones.
#[tokio::test]
async fn request_decode_errors_keep_the_json_cause_when_cloned() {
    let daemon = FakeDaemon::unix(|request| match request.target.as_str() {
        "/_ping" => ok(&[("Api-Version", "1.56")], "OK"),
        _ => ok(&[], r#"[{"ID":5}]"#),
    });
    let client = daemon.client();
    let error = client.get_list::<Node>("/nodes", "").await.unwrap_err();
    let cloned = error.clone();
    assert_eq!(error.kind(), ErrorKind::Decode);
    let cause = find_source::<serde_json::Error>(&error).expect("original JSON error");
    let cloned_cause = find_source::<serde_json::Error>(&cloned).expect("shared JSON error");
    assert!(cause.is_data());
    assert!(std::ptr::eq(cause, cloned_cause));
}

/// A connection failure keeps the `io::Error` of the dial as its cause.
#[tokio::test]
async fn connection_errors_keep_the_original_io_cause() {
    let dir = TempDir::new();
    let socket = dir.path().join("absent.sock").display().to_string();
    let client = Client::new(Settings::unix(&socket).unwrap());
    let error = list_containers(&client).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    let cause = find_source::<std::io::Error>(&error).expect("original connect error");
    assert_eq!(cause.kind(), std::io::ErrorKind::NotFound);
}

/// A daemon's JSON error body that is not JSON keeps serde's error as the cause.
#[tokio::test]
async fn response_decode_errors_keep_the_original_json_cause() {
    let daemon = FakeDaemon::unix(|request| match request.target.as_str() {
        "/_ping" => ok(&[("Api-Version", "1.56")], "OK"),
        _ => respond(500, &[("Content-Type", "application/json")], "{"),
    });
    let client = daemon.client();
    let error = list_containers(&client).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Response);
    assert_eq!(
        error.status(),
        Some(hyper::StatusCode::INTERNAL_SERVER_ERROR)
    );
    assert!(error.to_string().starts_with("error reading JSON: "));
    let cause = find_source::<serde_json::Error>(&error).expect("original response decode error");
    assert!(cause.is_eof());
}

/// A response-header timeout bounds a daemon that accepts and never answers; without one
/// (moby's default) only the caller bounds it.
#[tokio::test(start_paused = true)]
async fn the_response_header_timeout_is_the_caller_s() {
    let daemon = FakeDaemon::unix(|_| Reply::Silent);
    let socket = daemon.socket().to_owned();
    let bounded = Client::new(
        Settings::unix(&socket)
            .unwrap()
            .response_header_timeout(Some(Duration::from_millis(100))),
    );
    let error = bounded.ping().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert_eq!(
        error.to_string(),
        format!(
            "Cannot connect to the Docker daemon at unix://{socket}. Is the docker daemon running?"
        )
    );
    assert_eq!(daemon.wire(), ["HEAD /_ping", "GET /_ping"]);
    let unbounded = Client::new(Settings::unix(&socket).unwrap());
    let waited = tokio::time::timeout(Duration::from_secs(3600), unbounded.ping()).await;
    assert!(waited.is_err(), "moby's default client gave up by itself");
}

/// An answer that is not HTTP is net/http's malformed response, with moby's hint about a TLS
/// daemon.
#[tokio::test]
async fn a_malformed_answer_is_worded_as_moby() {
    let daemon = FakeDaemon::unix(|_| Reply::Raw(b"\x16\x03\x01\x00\x02\x02\x28".to_vec()));
    let client = daemon.client();
    let error = list_containers(&client).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert_eq!(
        error.to_string(),
        format!(
            "Get \"http://{}/v1.56/containers/json\": net/http: HTTP/1.x transport connection \
             broken: malformed HTTP response.\n* Are you trying to connect to a TLS-enabled daemon \
             without TLS?",
            daemon.socket().replace('/', "%2F")
        )
    );
    assert!(find_source::<hyper::Error>(&error).is_some());
}

/// A socket whose connections are accepted and never answered, held open by the returned task.
fn silent_daemon(dir: &TempDir) -> (String, tokio::task::JoinHandle<()>) {
    let socket = dir.path().join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let held = tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            open.push(stream);
        }
    });
    (socket.display().to_string(), held)
}

/// A call whose context is done before the daemon answers fails with net/http's `url.Error` for
/// the request, as moby passes it through; the texts are a Go program's against such a socket.
#[tokio::test(start_paused = true)]
async fn a_done_context_fails_the_request_as_net_http() {
    let dir = TempDir::new();
    let (socket, held) = silent_daemon(&dir);
    let client = Client::new(Settings::unix(&socket).unwrap());
    let url = format!(
        "http://{}/v1.56/containers/abc/json",
        socket.replace('/', "%2F")
    );
    let inspect = || client.get_object::<Node>("/containers/abc/json");

    let timed = CallContext::default().with_timeout(Duration::from_millis(200));
    let error = with_context(&timed, inspect()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::DeadlineExceeded);
    assert_eq!(
        error.to_string(),
        format!("Get \"{url}\": context deadline exceeded")
    );

    let error = crate::with_context_timeout(Duration::from_millis(200), inspect())
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("Get \"{url}\": context deadline exceeded")
    );

    // Without a context the client does not see, the bound is the bare error.
    let error = crate::with_bare_timeout(Duration::from_millis(200), inspect())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "context deadline exceeded");

    let cancelled = CallContext::default();
    let trigger = tokio::spawn({
        let cancelled = cancelled.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancelled.cancel();
        }
    });
    let error = with_context(&cancelled, inspect()).await.unwrap_err();
    trigger.await.unwrap();
    assert_eq!(error.kind(), ErrorKind::Canceled);
    assert_eq!(
        error.to_string(),
        format!("Get \"{url}\": context canceled")
    );
    held.abort();
}

/// Outside a client request, a done context is the bare error.
#[tokio::test(start_paused = true)]
async fn a_done_context_ends_any_other_call_with_the_bare_error() {
    let cancelled = CallContext::default();
    cancelled.cancel();
    let error = with_context(
        &cancelled,
        std::future::pending::<Result<(), crate::Error>>(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Canceled);
    assert_eq!(error.to_string(), "context canceled");
    assert_eq!(cancelled.err(), Some(ContextError::Canceled));

    let parent = CallContext::default().with_timeout(Duration::from_secs(1));
    let child = parent.with_timeout(Duration::from_secs(5));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(child.err(), Some(ContextError::DeadlineExceeded));
}

/// Go's context tree: a child's cancellation does not reach its parent, a parent's reaches its
/// children, and `err` keeps the cause that came first.
#[tokio::test(start_paused = true)]
async fn contexts_cancel_downwards_and_keep_the_first_cause() {
    let root = CallContext::new();
    let child = root.with_timeout(Duration::from_secs(5));
    child.cancel();
    assert_eq!(child.err(), Some(ContextError::Canceled));
    assert_eq!(
        root.err(),
        None,
        "a child's cancellation reached its parent"
    );

    let sibling = root.with_timeout(Duration::from_secs(5));
    root.cancel();
    assert_eq!(sibling.err(), Some(ContextError::Canceled));
    assert_eq!(sibling.done().await, ContextError::Canceled);

    // A deadline that passed before the cancellation stays the cause, and the other way round.
    let root = CallContext::new();
    let timed = root.with_timeout(Duration::from_secs(1));
    tokio::time::sleep(Duration::from_secs(2)).await;
    timed.cancel();
    assert_eq!(timed.err(), Some(ContextError::DeadlineExceeded));
    let timed = root.with_timeout(Duration::from_secs(1));
    root.cancel();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(timed.err(), Some(ContextError::Canceled));
}
