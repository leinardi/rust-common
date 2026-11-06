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

//! Streaming calls whose response headers can be awaited.
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

use std::time::Duration;

use bollard::errors::Error as BollardError;
use bollard::models::EventMessage;
use bollard::query_parameters::EventsOptions;
use bytes::Bytes;
use futures_util::Stream;
use http::header::{HOST, USER_AGENT};
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt as _, Empty};
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

/// Sends `method path?query` to `target` and returns the response once its headers are in and say
/// success, its body unread. `timeout` bounds the connection, the headers and, for a refusal, the
/// refusal's body; `cancel` ends it at once.
///
/// # Errors
///
/// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when `timeout` passes first; the engine's
/// status as bollard maps it (404 is `NotFound`, 409 `Conflict`) for a non-2xx answer; the
/// connection's failure as `Unavailable`.
pub(crate) async fn establish(
    config: &Config,
    target: &Target,
    method: Method,
    path: &str,
    query: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Incoming, Error> {
    let mut uri = versioned_path(config, path);
    if !query.is_empty() {
        uri.push('?');
        uri.push_str(query);
    }
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(HOST, target.host());
    if let Some(agent) = &config.user_agent {
        request = request.header(USER_AGENT, agent.as_str());
    }
    let request = request
        .body(Empty::<Bytes>::new())
        .map_err(|error| Error::from(BollardError::from(error)))?;
    let answered = async {
        let response = match target {
            #[cfg(feature = "tls")]
            Target::Tls { addr, certs } => {
                let certs = certs.clone();
                let connector = tokio::task::spawn_blocking(move || tls_connector(&certs))
                    .await
                    .map_err(|error| failure(error.to_string()))??;
                let name = rustls_pki_types::ServerName::try_from(host_of(addr).to_owned())
                    .map_err(|error| failure(format!("TLS server name {addr:?}: {error}")))?;
                let tls = connector.connect(name, connect_tcp(addr).await?).await?;
                send(TokioIo::new(tls), request).await?
            }
            Target::Unix(path) => {
                send(TokioIo::new(UnixStream::connect(path).await?), request).await?
            }
            #[cfg(feature = "tcp")]
            Target::Tcp(addr) => send(TokioIo::new(connect_tcp(addr).await?), request).await?,
        };
        let status = response.status();
        if status.is_success() {
            return Ok(response.into_body());
        }
        let body = response.into_body().collect().await?.to_bytes();
        Err(refusal(status, &body))
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::cancelled()),
        bounded = tokio::time::timeout(timeout, answered) => match bounded {
            Ok(result) => result.map_err(|error: BollardError| Error::from(error)),
            Err(_elapsed) => Err(Error::timeout(timeout)),
        },
    }
}

#[cfg(feature = "tls")]
fn failure(message: String) -> BollardError {
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
    request: Request<Empty<Bytes>>,
) -> Result<http::Response<Incoming>, BollardError>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await?;
    // Ends with the response body; a failure surfaces through the body or the response.
    tokio::spawn(async move {
        let _ = connection.await;
    });
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
        Method::GET,
        "/events",
        &query,
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
