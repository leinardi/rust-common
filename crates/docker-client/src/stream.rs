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

//! Calls sent on docker-client's own connection: streaming ones whose response headers can be
//! awaited, ones that take the connection over, and plain ones whose JSON body bollard cannot
//! express.
//!
//! bollard hides the moment a streaming call's response headers arrive: its stream is lazy, and
//! its first item is the first body chunk. For a wait (the engine has the waiter once it answers
//! the headers) and for an event subscription (the engine accepted it) that moment is the one a
//! caller needs, as moby's Go client gave it. This sends the request itself, over hyper on the
//! same endpoint the [`Config`] describes, with the same `User-Agent` and API version prefix, and
//! returns once the headers are in.
//!
//! A TLS endpoint (`tls` feature) is spoken to with rustls on the material bollard's ssl path
//! trusts and presents: the system roots plus `ca.pem`, and `cert.pem` and `key.pem` from the
//! config's certificate directory, so bollard's calls and these accept the same engines.

use std::future::Future;
use std::time::Duration;

use bollard::errors::Error as BollardError;
use bollard::models::EventMessage;
use bollard::query_parameters::EventsOptions;
use bytes::Bytes;
use futures_util::Stream;
use http::header::{CONNECTION, CONTENT_TYPE, HOST, UPGRADE, USER_AGENT};
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt as _, Either, Empty, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, Endpoint, sent_version};
use crate::error::Error;

/// Where the engine listens, for the endpoints this module serves.
pub(crate) enum Target {
    Unix(String),
    #[cfg(feature = "tcp")]
    Tcp(String),
    #[cfg(feature = "tls")]
    Tls {
        addr: String,
        certs: std::path::PathBuf,
    },
}

impl Target {
    /// The target of `config`.
    pub(crate) fn of(config: &Config) -> Self {
        match &config.endpoint {
            Endpoint::Unix(path) => Self::Unix(path.clone()),
            #[cfg(feature = "tcp")]
            Endpoint::Tcp(addr) => Self::Tcp(addr.trim_start_matches("tcp://").to_owned()),
            #[cfg(feature = "tls")]
            Endpoint::Tls { addr, certs } => Self::Tls {
                addr: addr.trim_start_matches("tcp://").to_owned(),
                certs: certs.clone(),
            },
        }
    }

    fn host(&self) -> &str {
        match self {
            Self::Unix(_) => "docker",
            #[cfg(feature = "tls")]
            Self::Tls { addr, .. } => addr,
            #[cfg(feature = "tcp")]
            Self::Tcp(addr) => addr,
        }
    }
}

/// The path the client sends `path` under, with the version prefix its own calls carry
/// ([`sent_version`]).
fn versioned_path(config: &Config, path: &str) -> String {
    sent_version(config).map_or_else(
        || path.to_owned(),
        |version| {
            format!(
                "/v{}.{}{path}",
                version.major_version, version.minor_version
            )
        },
    )
}

/// A request to the engine: its method, path (without the version prefix), query string (without
/// its `?`, empty for none) and JSON body.
pub(crate) struct Call<'call> {
    pub(crate) method: Method,
    pub(crate) path: &'call str,
    pub(crate) query: &'call str,
    pub(crate) body: Option<Bytes>,
}

impl<'call> Call<'call> {
    /// `method path?query` with no body.
    pub(crate) const fn new(method: Method, path: &'call str, query: &'call str) -> Self {
        Self {
            method,
            path,
            query,
            body: None,
        }
    }

    #[cfg(any(feature = "exec", feature = "raw-spec"))]
    /// The same request with `json` as its body, sent with `Content-Type: application/json`.
    pub(crate) fn json(self, json: String) -> Self {
        Self {
            body: Some(Bytes::from(json)),
            ..self
        }
    }
}

/// The body of a request: none (no `Content-Length`, as the calls before the exec ones sent), or
/// a JSON document whose length hyper announces.
type RequestBody = Either<Empty<Bytes>, Full<Bytes>>;

/// Whether the connection may be taken over after a `101`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plain,
    Upgrade,
}

/// Sends `call` to `target` and returns the response once its headers are in and say success, its
/// body unread. `timeout` bounds the connection, the headers and, for a refusal, the refusal's
/// body; `cancel` ends it at once.
///
/// # Errors
///
/// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when `timeout` passes first; the engine's
/// status as bollard maps it (404 is `NotFound`, 409 `Conflict`) for a non-2xx answer; the
/// connection's failure as `Unavailable`.
pub(crate) async fn establish(
    config: &Config,
    target: &Target,
    call: Call<'_>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Incoming, Error> {
    let request = request(config, target, call, Mode::Plain)?;
    guarded(timeout, cancel, async {
        let response = exchange(target, request, Mode::Plain).await?;
        accepted(response).await
    })
    .await
}

/// Sends `call` to `target` and returns the whole body of its successful answer. `timeout` bounds
/// all of it, the body included; `cancel` ends it at once.
///
/// # Errors
///
/// As [`establish`], with [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) also for a body that
/// does not end in `timeout`.
#[cfg(any(feature = "exec", feature = "stats", feature = "raw-spec"))]
pub(crate) async fn complete(
    config: &Config,
    target: &Target,
    call: Call<'_>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Bytes, Error> {
    let request = request(config, target, call, Mode::Plain)?;
    guarded(timeout, cancel, async {
        let response = exchange(target, request, Mode::Plain).await?;
        Ok(accepted(response).await?.collect().await?.to_bytes())
    })
    .await
}

/// Sends `call` to `target` asking to take the connection over (`Connection: Upgrade`,
/// `Upgrade: tcp`, as moby's hijack does) and returns the connection once the engine has answered
/// `101`. `timeout` bounds the connection and the answer, not the session; `cancel` ends the
/// establishment at once.
///
/// # Errors
///
/// As [`establish`]; an answer other than `101` is an error with that status (so 404 is
/// `NotFound`, 409 `Conflict`, a `200` `Other`) and moby's text, `unable to upgrade to tcp,
/// received <status>`.
#[cfg(feature = "exec")]
pub(crate) async fn upgrade(
    config: &Config,
    target: &Target,
    call: Call<'_>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<hyper::upgrade::Upgraded, Error> {
    let request = request(config, target, call, Mode::Upgrade)?;
    guarded(timeout, cancel, async {
        let response = exchange(target, request, Mode::Upgrade).await?;
        let status = response.status();
        if status != StatusCode::SWITCHING_PROTOCOLS {
            return Err(BollardError::DockerResponseServerError {
                status_code: status.as_u16(),
                message: format!("unable to upgrade to tcp, received {}", status.as_u16()),
            });
        }
        Ok(hyper::upgrade::on(response).await?)
    })
    .await
}

/// The request for `call`, with the client's version prefix and `User-Agent`.
fn request(
    config: &Config,
    target: &Target,
    call: Call<'_>,
    mode: Mode,
) -> Result<Request<RequestBody>, Error> {
    let mut uri = versioned_path(config, call.path);
    if !call.query.is_empty() {
        uri.push('?');
        uri.push_str(call.query);
    }
    let mut request = Request::builder()
        .method(call.method)
        .uri(uri)
        .header(HOST, target.host());
    if let Some(agent) = &config.user_agent {
        request = request.header(USER_AGENT, agent.as_str());
    }
    if mode == Mode::Upgrade {
        request = request.header(CONNECTION, "Upgrade").header(UPGRADE, "tcp");
    }
    let body = match call.body {
        Some(json) => {
            request = request.header(CONTENT_TYPE, "application/json");
            Either::Right(Full::new(json))
        }
        None => Either::Left(Empty::new()),
    };
    request
        .body(body)
        .map_err(|error| Error::from(BollardError::from(error)))
}

/// Runs `exchange` under `timeout` and `cancel`, `cancel` winning a tie.
async fn guarded<T>(
    timeout: Duration,
    cancel: &CancellationToken,
    exchange: impl Future<Output = Result<T, BollardError>>,
) -> Result<T, Error> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::cancelled()),
        bounded = tokio::time::timeout(timeout, exchange) => match bounded {
            Ok(result) => result.map_err(Error::from),
            Err(_elapsed) => Err(Error::timeout(timeout)),
        },
    }
}

/// The body of a successful `response`, or the refusal its status and body make.
async fn accepted(response: http::Response<Incoming>) -> Result<Incoming, BollardError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response.into_body());
    }
    let body = response.into_body().collect().await?.to_bytes();
    Err(refusal(status, &body))
}

/// Connects to `target` and sends `request` on that connection.
async fn exchange(
    target: &Target,
    request: Request<RequestBody>,
    mode: Mode,
) -> Result<http::Response<Incoming>, BollardError> {
    match target {
        #[cfg(feature = "tls")]
        Target::Tls { addr, certs } => {
            let certs = certs.clone();
            let connector = tokio::task::spawn_blocking(move || tls_connector(&certs))
                .await
                .map_err(|error| failure(error.to_string()))??;
            let name = rustls_pki_types::ServerName::try_from(host_of(addr).to_owned())
                .map_err(|error| failure(format!("TLS server name {addr:?}: {error}")))?;
            let tls = connector.connect(name, connect_tcp(addr).await?).await?;
            send(TokioIo::new(tls), request, mode).await
        }
        Target::Unix(path) => {
            send(
                TokioIo::new(UnixStream::connect(path).await?),
                request,
                mode,
            )
            .await
        }
        #[cfg(feature = "tcp")]
        Target::Tcp(addr) => send(TokioIo::new(connect_tcp(addr).await?), request, mode).await,
    }
}

/// `id` without surrounding whitespace, or the refusal of an empty one, as moby's client's
/// `trimID` does before the calls that use it; `kind` names the object (`container`, `exec`,
/// `service`).
#[cfg(any(
    feature = "exec",
    feature = "logs",
    feature = "stats",
    feature = "raw-spec"
))]
pub(crate) fn trim_id<'id>(kind: &str, id: &'id str) -> Result<&'id str, Error> {
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::from(failure(format!(
            "invalid {kind} name or ID: value is empty"
        ))));
    }
    Ok(id)
}

/// `id` as moby's client puts it into a request path: Go's `url.URL.EscapedPath` writes every byte
/// but an ASCII letter, a digit and `-_.~$&+,/:;=@` as `%XX` with upper-case hex. An ID holding
/// `?`, `#`, `%` or a space so names that ID to the engine, which answers 404, rather than another
/// endpoint or another ID. `/` stays, as in Go. moby's `path.Join` also cleans the path (dot
/// segments, repeated and trailing slashes), which is not done here, so an ID holding `/` or one of
/// `.` or `..` stays the caller's to keep out.
#[cfg(any(
    feature = "attach",
    feature = "exec",
    feature = "logs",
    feature = "stats",
    feature = "raw-spec"
))]
pub(crate) fn escape_id(id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut escaped = String::with_capacity(id.len());
    for &byte in id.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~$&+,/:;=@".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push('%');
            escaped.push(char::from(HEX[usize::from(byte >> 4)]));
            escaped.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    escaped
}

/// A failure of the client's own making, in bollard's error.
#[cfg(any(
    feature = "tls",
    feature = "exec",
    feature = "logs",
    feature = "stats",
    feature = "raw-spec"
))]
pub(crate) fn failure(message: String) -> BollardError {
    BollardError::DockerStreamError { error: message }
}

/// The host of a `host:port` address, without the brackets of an IPv6 literal.
#[cfg(feature = "tls")]
fn host_of(addr: &str) -> &str {
    let host = addr.rsplit_once(':').map_or(addr, |(host, _port)| host);
    host.trim_start_matches('[').trim_end_matches(']')
}

/// The TLS client for `certs`, trusting what bollard's ssl path trusts so that its calls and the
/// streaming ones accept the same engines: the system roots (a root store that fails to load is
/// an error, as there) plus `ca.pem` to verify the engine, `cert.pem` and `key.pem` to present.
#[cfg(feature = "tls")]
fn tls_connector(certs: &std::path::Path) -> Result<tokio_rustls::TlsConnector, BollardError> {
    use rustls_pki_types::pem::PemObject as _;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};

    let chain = |name: &str| -> Result<Vec<CertificateDer<'static>>, BollardError> {
        let path = certs.join(name);
        CertificateDer::pem_file_iter(&path)
            .and_then(Iterator::collect)
            .map_err(|error| failure(format!("{}: {error}", path.display())))
    };
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    if let Some(error) = native.errors.first() {
        return Err(failure(format!("the system root certificates: {error}")));
    }
    for root in native.certs {
        roots
            .add(root)
            .map_err(|error| failure(format!("a system root certificate: {error}")))?;
    }
    roots.add_parsable_certificates(chain("ca.pem")?);
    let key_path = certs.join("key.pem");
    let key = PrivateKeyDer::from_pem_file(&key_path)
        .map_err(|error| failure(format!("{}: {error}", key_path.display())))?;
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| failure(error.to_string()))?
    .with_root_certificates(roots)
    .with_client_auth_cert(chain("cert.pem")?, key)
    .map_err(|error| failure(error.to_string()))?;
    Ok(tokio_rustls::TlsConnector::from(std::sync::Arc::new(
        config,
    )))
}

/// A TCP connection to `addr` with keepalive on (15 seconds idle and apart, nine probes where the
/// platform counts them, as docker's own client), so a remote engine that vanishes ends a pending
/// wait or event stream instead of leaving it hanging forever.
#[cfg(feature = "tcp")]
pub(crate) async fn connect_tcp(addr: &str) -> std::io::Result<tokio::net::TcpStream> {
    let stream = tokio::net::TcpStream::connect(addr).await?;
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(15))
        .with_interval(Duration::from_secs(15));
    #[cfg(target_os = "linux")]
    let keepalive = keepalive.with_retries(9);
    socket2::SockRef::from(&stream).set_tcp_keepalive(&keepalive)?;
    Ok(stream)
}

async fn send<I>(
    io: I,
    request: Request<RequestBody>,
    mode: Mode,
) -> Result<http::Response<Incoming>, BollardError>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await?;
    // Ends with the response body, or hands the connection over after a `101`; a failure surfaces
    // through the body, the response or the upgrade.
    match mode {
        Mode::Plain => {
            tokio::spawn(async move {
                let _ = connection.await;
            });
        }
        Mode::Upgrade => {
            tokio::spawn(async move {
                let _ = connection.with_upgrades().await;
            });
        }
    }
    Ok(sender.send_request(request).await?)
}

fn refusal(status: StatusCode, body: &[u8]) -> BollardError {
    let message = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    BollardError::DockerResponseServerError {
        status_code: status.as_u16(),
        message,
    }
}

/// The events stream of [`events_established`].
pub type EventStream = std::pin::Pin<Box<dyn Stream<Item = Result<EventMessage, Error>> + Send>>;

/// `GET /events` with `options`, returned once the engine has answered 2xx: the subscription is
/// accepted, not just sent. `timeout` bounds the connection and the headers, not the stream;
/// `cancel` interrupts the establishment (the stream itself ends when it is dropped).
///
/// `config` is the client's: the one it was connected from, or the one
/// [`negotiate`](crate::negotiate) returned with it. The request carries its `User-Agent` and API
/// version, as the client's own.
///
/// Each line of the stream is decoded as an [`EventMessage`]; one that does not decode is an `Err`
/// item and the stream goes on. A transport failure is an `Err` item that ends it.
///
/// # Errors
///
/// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when the headers do not come in `timeout`,
/// [`ErrorKind::Cancelled`](crate::ErrorKind::Cancelled) when `cancel` fires first, the engine's
/// refusal sorted into its kind (404, 409, other), `Unavailable` when it cannot be reached.
pub async fn events_established(
    config: &Config,
    options: Option<EventsOptions>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<EventStream, Error> {
    let target = Target::of(config);
    let query = options
        .map(|options| serde_urlencoded::to_string(&options))
        .transpose()
        .map_err(|error| Error::from(BollardError::from(error)))?
        .unwrap_or_default();
    let body = establish(
        config,
        &target,
        Call::new(Method::GET, "/events", &query),
        timeout,
        cancel,
    )
    .await?;
    Ok(Box::pin(lines(body)))
}

fn lines(body: Incoming) -> impl Stream<Item = Result<EventMessage, Error>> + Send {
    futures_util::stream::unfold(
        (Some(body), Vec::<u8>::new()),
        |(mut body, mut buffer)| async move {
            loop {
                let line = if let Some(at) = buffer.iter().position(|byte| *byte == b'\n') {
                    buffer.drain(..=at).collect::<Vec<u8>>()
                } else if body.is_none() {
                    std::mem::take(&mut buffer)
                } else {
                    match body.as_mut()?.frame().await {
                        Some(Ok(frame)) => {
                            if let Ok(data) = frame.into_data() {
                                buffer.extend_from_slice(&data);
                            }
                            continue;
                        }
                        Some(Err(error)) => {
                            let item = Err(Error::from(BollardError::from(error)));
                            return Some((item, (None, Vec::new())));
                        }
                        None => {
                            body = None;
                            continue;
                        }
                    }
                };
                let line = line.trim_ascii();
                if line.is_empty() {
                    if body.is_none() && buffer.is_empty() {
                        return None;
                    }
                    continue;
                }
                let item = serde_json::from_slice::<EventMessage>(line)
                    .map_err(|error| Error::from(BollardError::from(error)));
                return Some((item, (body, buffer)));
            }
        },
    )
}

#[cfg(all(
    test,
    any(
        feature = "attach",
        feature = "exec",
        feature = "logs",
        feature = "stats",
        feature = "raw-spec"
    )
))]
mod escape_tests {
    use super::escape_id;

    /// Each ID against the path segment Go 1.26 writes for it with
    /// `url.URL{Path: path.Join("/v1.44", "/containers/"+id+"/logs")}.String()`.
    #[test]
    fn ids_are_escaped_as_go_writes_them() {
        for (id, go) in [
            ("abc123", "abc123"),
            ("a?b", "a%3Fb"),
            ("a#b", "a%23b"),
            ("a%b", "a%25b"),
            ("a b", "a%20b"),
            ("a/b", "a/b"),
            ("név", "n%C3%A9v"),
            (
                "x!'()*[]{}|\\^`\"<>",
                "x%21%27%28%29%2A%5B%5D%7B%7D%7C%5C%5E%60%22%3C%3E",
            ),
            ("-_.~$&+,:;=@", "-_.~$&+,:;=@"),
            ("\t\n\x7f\x00", "%09%0A%7F%00"),
        ] {
            assert_eq!(escape_id(id), go, "{id:?}");
        }
    }
}
