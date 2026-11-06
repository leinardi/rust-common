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
//! A TLS endpoint is not served here (the handshake would repeat bollard's rustls setup): callers
//! fall back to bollard's lazy stream for it, and the call then returns before the engine has
//! answered anything.

use std::time::Duration;

use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::models::EventMessage;
use bollard::query_parameters::EventsOptions;
use bytes::Bytes;
use futures_util::{Stream, StreamExt as _};
use http::header::{HOST, USER_AGENT};
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;

use crate::config::{Config, Endpoint, sent_version};
use crate::error::Error;

/// Where the engine listens, for the endpoints this module serves.
pub(crate) enum Target {
    Unix(String),
    #[cfg(feature = "tcp")]
    Tcp(String),
    /// An endpoint this module does not serve (TLS): the caller uses bollard's lazy stream.
    #[cfg(feature = "tls")]
    Lazy,
}

impl Target {
    /// The target of `config`, [`Target::Lazy`] for a TLS endpoint.
    pub(crate) fn of(config: &Config) -> Self {
        match &config.endpoint {
            Endpoint::Unix(path) => Self::Unix(path.clone()),
            #[cfg(feature = "tcp")]
            Endpoint::Tcp(addr) => Self::Tcp(addr.trim_start_matches("tcp://").to_owned()),
            #[cfg(feature = "tls")]
            Endpoint::Tls { .. } => Self::Lazy,
        }
    }

    /// Whether the caller must use bollard's lazy stream.
    pub(crate) fn is_lazy(&self) -> bool {
        match self {
            Self::Unix(_) => false,
            #[cfg(feature = "tcp")]
            Self::Tcp(_) => false,
            #[cfg(feature = "tls")]
            Self::Lazy => true,
        }
    }

    fn host(&self) -> &str {
        match self {
            Self::Unix(_) => "docker",
            #[cfg(feature = "tls")]
            Self::Lazy => "docker",
            #[cfg(feature = "tcp")]
            Self::Tcp(addr) => addr,
        }
    }
}

/// The path the client sends `path` under, with the version prefix its own calls carry
/// ([`sent_version`]).
fn versioned_path(docker: &Docker, config: &Config, path: &str) -> String {
    sent_version(config, docker.client_version()).map_or_else(
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
/// refusal's body.
///
/// # Errors
///
/// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when `timeout` passes first; the engine's
/// status as bollard maps it (404 is `NotFound`, 409 `Conflict`) for a non-2xx answer; the
/// connection's failure as `Unavailable`.
pub(crate) async fn establish(
    docker: &Docker,
    config: &Config,
    target: &Target,
    method: Method,
    path: &str,
    query: &str,
    timeout: Duration,
) -> Result<Incoming, Error> {
    let mut uri = versioned_path(docker, config, path);
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
            Target::Lazy => {
                return Err(BollardError::DockerStreamError {
                    error: "this endpoint has no header-awaiting path".to_owned(),
                });
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
    match tokio::time::timeout(timeout, answered).await {
        Ok(result) => result.map_err(|error: BollardError| Error::from(error)),
        Err(_elapsed) => Err(Error::timeout(timeout)),
    }
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
/// accepted, not just sent. `timeout` bounds the connection and the headers, not the stream.
///
/// `docker` and `config` must be a client and the [`Config`] it was connected from; the request
/// carries the config's `User-Agent` and the client's API version.
///
/// Each line of the stream is decoded as an [`EventMessage`]; one that does not decode is an `Err`
/// item and the stream goes on. A transport failure is an `Err` item that ends it.
///
/// A TLS endpoint is served by bollard's lazy stream instead: the call returns at once, and
/// nothing says the subscription was accepted.
///
/// # Errors
///
/// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when the headers do not come in `timeout`,
/// the engine's refusal sorted into its kind (404, 409, other), `Unavailable` when it cannot be
/// reached.
pub async fn events_established(
    docker: &Docker,
    config: &Config,
    options: Option<EventsOptions>,
    timeout: Duration,
) -> Result<EventStream, Error> {
    let target = Target::of(config);
    if target.is_lazy() {
        return Ok(Box::pin(
            docker.events(options).map(|item| item.map_err(Error::from)),
        ));
    }
    let query = options
        .map(|options| serde_urlencoded::to_string(&options))
        .transpose()
        .map_err(|error| Error::from(BollardError::from(error)))?
        .unwrap_or_default();
    let body = establish(
        docker,
        config,
        &target,
        Method::GET,
        "/events",
        &query,
        timeout,
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
