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

//! The client: its settings, the one request builder, the ping and moby's lazy API version
//! negotiation, the generic calls, and moby's error wording.
//!
//! Every request carries the headers moby client v0.6.0 sends, in Go's order: `Host`,
//! `User-Agent`, the `Accept` lines, then on TCP the transport's `Accept-Encoding: gzip`. The
//! `User-Agent` is the caller's ([`Settings::user_agent`]); without one it is moby's own,
//! `moby-client/v0.6.0 <GOOS>/<GOARCH>` ([`moby_user_agent`]).
//!
//! Extracted from swarm-scheduler-exporter/src/docker/client.rs at c9e79afa8810 (`from_env`, TCP,
//! TLS, gzip, the response-header timeout, the causes).
//! Extracted from swarm-device-access/src/docker/client.rs at 8b4ab3a3e417 (`Settings::unix`, the
//! context, the ping's bounded drain, `exchange`, `exchange_upgrade`, `go_arch`).
//! Extracted from swarm-scheduler-exporter/src/docker/api.rs at c9e79afa8810 (`get_body`,
//! `get_list`, `get_object`, the events request).

#[cfg(feature = "tcp")]
use std::io::Write as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
#[cfg(feature = "tcp")]
use flate2::write::GzDecoder;
use go_compat::errno::io_error_text;
use go_compat::http_status::status_text;
use go_compat::quote::quote;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::body::{Frame, SizeHint};
use hyper::header::{ACCEPT, CONTENT_TYPE, HOST, HeaderValue, USER_AGENT};
#[cfg(feature = "tcp")]
use hyper::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH};
use hyper::{Method, Request, Response, StatusCode};
#[cfg(feature = "tls")]
use rustls::ClientConfig;
use serde::Deserialize;

use crate::connect::Connector;
use crate::context;
use crate::error::{ConfigError, Error};
use crate::events::{EVENTS_ACCEPT, JsonStream};
use crate::host::{Endpoint, escape_host, escape_path, join_path, unix_socket};
#[cfg(feature = "tcp")]
use crate::host::{Host, remove_zone};
use crate::pool::{Pool, PooledBody, RequestBody, SendError};
use crate::types::{decode_first, decode_list};
use crate::version::{self, MAX_API_VERSION};
#[cfg(feature = "tcp")]
use crate::{DEFAULT_DOCKER_HOST, version::parse_api_version};
use crate::{DUMMY_HOST, MOBY_CLIENT_VERSION};

/// moby's `checkResponseErr` reads at most this much of an error body.
const ERROR_BODY_LIMIT: usize = 1024 * 1024;

/// moby's `ensureReaderClosed`: how much of a body that is not read is drained, so the
/// connection is reused.
const DRAIN_LIMIT: usize = 512;

/// Go's `runtime.GOARCH` for the architecture the binary runs on, as moby's `User-Agent` names
/// it.
#[must_use]
pub fn go_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "powerpc64" if cfg!(target_endian = "little") => "ppc64le",
        "powerpc64" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    }
}

/// moby's default `User-Agent`: `moby-client/v0.6.0 <GOOS>/<GOARCH>`.
#[must_use]
pub fn moby_user_agent() -> String {
    format!(
        "moby-client/{MOBY_CLIENT_VERSION} {}/{}",
        std::env::consts::OS,
        go_arch()
    )
}

/// Where the daemon is and how the client talks to it.
///
/// Two settings are equal when every field is; with `tls`, their TLS configurations are equal only
/// when they are the same one (an [`Arc`] clone), as rustls's `ClientConfig` cannot be compared.
#[derive(Debug, Clone)]
pub struct Settings {
    /// moby's `DaemonHost`, as given.
    host: String,
    /// The address errors name: the socket path, or the TCP authority.
    addr: String,
    /// The path of a `tcp://` URL, which prefixes every request path.
    base_path: String,
    endpoint: Endpoint,
    #[cfg(feature = "tls")]
    tls: Option<Arc<ClientConfig>>,
    version: String,
    pinned: bool,
    user_agent: String,
    /// `user_agent`, checked as a header value when it was set.
    user_agent_header: HeaderValue,
    response_header_timeout: Option<Duration>,
}

impl PartialEq for Settings {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(feature = "tls")]
        let same_tls = match (&self.tls, &other.tls) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        #[cfg(not(feature = "tls"))]
        let same_tls = true;
        same_tls
            && self.host == other.host
            && self.addr == other.addr
            && self.base_path == other.base_path
            && self.endpoint == other.endpoint
            && self.version == other.version
            && self.pinned == other.pinned
            && self.user_agent == other.user_agent
            && self.response_header_timeout == other.response_header_timeout
    }
}

impl Eq for Settings {}

impl Settings {
    fn with_endpoint(host: String, addr: String, base_path: String, endpoint: Endpoint) -> Self {
        Self {
            host,
            addr,
            base_path,
            endpoint,
            #[cfg(feature = "tls")]
            tls: None,
            version: MAX_API_VERSION.to_owned(),
            pinned: false,
            user_agent: moby_user_agent(),
            // moby's own is `moby-client/<version> <GOOS>/<GOARCH>`, all visible ASCII.
            user_agent_header: HeaderValue::from_str(&moby_user_agent())
                .unwrap_or_else(|_| HeaderValue::from_static("moby-client")),
            response_header_timeout: None,
        }
    }

    /// moby's `New(WithHost("unix://" + socket))`: the socket path, refused when empty or longer
    /// than the kernel takes, with moby's defaults (its `User-Agent`, no response-header
    /// timeout, the version negotiated).
    ///
    /// # Errors
    ///
    /// [`ConfigError`] with moby's message, without a prefix.
    pub fn unix(socket: &str) -> Result<Self, ConfigError> {
        let socket = unix_socket(socket).map_err(ConfigError::new)?;
        Ok(Self::with_endpoint(
            format!("unix://{socket}"),
            socket.clone(),
            String::new(),
            Endpoint::Unix(socket),
        ))
    }

    /// The environment as moby's `FromEnv` reads it (on a transport the client owns), with
    /// `getenv` for `os.Getenv` (unset and empty are the same): with `tls`, TLS from
    /// `DOCKER_CERT_PATH` and `DOCKER_TLS_VERIFY` (an empty `DOCKER_TLS_VERIFY` is refused instead
    /// of connecting without verifying the daemon); then the host, [`DEFAULT_DOCKER_HOST`] unless
    /// `DOCKER_HOST` sets one; then `DOCKER_API_VERSION`, trimmed and without a `v` prefix, which
    /// pins the version and skips negotiation. The range of a pinned version is checked by
    /// [`version::validate_client_api_version`], if the caller wants it. Without `tls`, a
    /// `DOCKER_CERT_PATH` is refused rather than ignored.
    ///
    /// # Errors
    ///
    /// [`ConfigError`]: `configure TLS from DOCKER_CERT_PATH=<path>: …` for the TLS material, and
    /// `docker client: …` for a host or API version moby refuses.
    #[cfg(feature = "tcp")]
    pub fn from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &str| getenv(name).filter(|value| !value.is_empty());
        #[cfg(feature = "tls")]
        let tls = match get("DOCKER_CERT_PATH") {
            Some(cert_path) => Some(Arc::new(crate::tls::client_config(
                &cert_path,
                get("DOCKER_TLS_VERIFY").is_some(),
            )?)),
            None => None,
        };
        #[cfg(not(feature = "tls"))]
        if let Some(cert_path) = get("DOCKER_CERT_PATH") {
            return Err(ConfigError::new(format!(
                "configure TLS from DOCKER_CERT_PATH={cert_path}: TLS is not built in (the \
                 docker-engine `tls` feature)"
            )));
        }
        let host = get("DOCKER_HOST").unwrap_or_else(|| DEFAULT_DOCKER_HOST.to_owned());
        let docker_client = |message: String| ConfigError::new(message).context("docker client");
        let parsed = Host::parse(&host).map_err(docker_client)?;
        let endpoint = parsed.endpoint().map_err(docker_client)?;
        let raw_version = getenv("DOCKER_API_VERSION").unwrap_or_default();
        let raw_version = raw_version.trim();
        let pin = raw_version.strip_prefix('v').unwrap_or(raw_version);
        let pinned = if pin.is_empty() {
            None
        } else {
            Some(parse_api_version(pin).map_err(|reason| {
                docker_client(format!("invalid API version ({raw_version}): {reason}"))
            })?)
        };
        let mut settings = Self::with_endpoint(host, parsed.addr, parsed.base_path, endpoint);
        #[cfg(feature = "tls")]
        {
            settings.tls = tls;
        }
        if let Some(version) = pinned {
            settings.version = version;
            settings.pinned = true;
        }
        Ok(settings)
    }

    /// The `User-Agent` of every request, in place of moby's ([`moby_user_agent`]).
    ///
    /// A value no HTTP header may carry (a control character other than a tab, such as a line
    /// break) is refused here, rather than replaced by another `User-Agent` behind the caller's
    /// back: Go's net/http fails every request that carries it, with the same message. A caller
    /// that wants a fallback (swarm-scheduler-exporter's bare program name for a version stamp
    /// that cannot be sent) checks its value with [`Settings::check_user_agent`] first.
    ///
    /// # Errors
    ///
    /// [`ConfigError`]: `net/http: invalid header field value for "User-Agent"`.
    pub fn user_agent(mut self, user_agent: String) -> Result<Self, ConfigError> {
        self.user_agent_header = Self::check_user_agent(&user_agent)?;
        self.user_agent = user_agent;
        Ok(self)
    }

    /// Whether `user_agent` can be sent as a `User-Agent`, by Go's rule (`httpguts`'s
    /// `ValidHeaderFieldValue`: no control character but a tab), and the header it makes.
    ///
    /// # Errors
    ///
    /// [`ConfigError`]: `net/http: invalid header field value for "User-Agent"`.
    pub fn check_user_agent(user_agent: &str) -> Result<HeaderValue, ConfigError> {
        HeaderValue::from_str(user_agent).map_err(|_| {
            ConfigError::new("net/http: invalid header field value for \"User-Agent\"".to_owned())
        })
    }

    /// The longest wait for a response's headers, from the request written (Go's
    /// `Transport.ResponseHeaderTimeout`), or `None` for moby's default, no bound: the caller
    /// bounds every call. It also bounds each body read of the ping the negotiation sends
    /// ([`Client::ping`]), which every request waits on.
    #[must_use]
    pub fn response_header_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.response_header_timeout = timeout;
        self
    }

    /// moby's `DaemonHost`: `unix://<socket>`, or `DOCKER_HOST` (the default without one).
    #[must_use]
    pub fn daemon_host(&self) -> &str {
        &self.host
    }

    /// The socket path, for a unix socket.
    #[must_use]
    pub fn socket(&self) -> Option<&str> {
        match &self.endpoint {
            Endpoint::Unix(socket) => Some(socket),
            #[cfg(feature = "tcp")]
            Endpoint::Tcp(_) => None,
        }
    }

    /// moby's `ClientVersion` before any request: the pinned version, or [`MAX_API_VERSION`].
    #[must_use]
    pub fn client_version(&self) -> &str {
        &self.version
    }

    /// The `User-Agent` requests carry.
    #[must_use]
    pub fn user_agent_value(&self) -> &str {
        &self.user_agent
    }
}

/// The negotiated API version, shared by every request.
#[derive(Debug)]
struct Negotiation {
    state: tokio::sync::Mutex<NegotiationState>,
    // Bumped after every ping, so a request that waited for one shares its outcome.
    pings: AtomicU64,
}

#[derive(Debug)]
struct NegotiationState {
    version: String,
    negotiated: bool,
}

/// The Docker client.
#[derive(Debug)]
pub struct Client {
    settings: Settings,
    pool: Arc<Pool>,
    scheme: &'static str,
    host_header: HeaderValue,
    user_agent: HeaderValue,
    #[cfg(feature = "tcp")]
    gzip: bool,
    negotiation: Negotiation,
}

/// A response body: read it to the end and the connection is reused. On TCP a gzip body is
/// decompressed as Go's transport does when it asked for gzip itself.
#[derive(Debug)]
pub struct Body {
    inner: PooledBody,
    #[cfg(feature = "tcp")]
    gzip: Option<GzDecoder<Vec<u8>>>,
}

impl hyper::body::Body for Body {
    type Data = Bytes;
    type Error = Error;

    #[cfg(not(feature = "tcp"))]
    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        Pin::new(&mut self.inner)
            .poll_frame(context)
            .map(|frame| frame.map(|frame| frame.map_err(body_error)))
    }

    #[cfg(feature = "tcp")]
    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        loop {
            let frame = match Pin::new(&mut self.inner).poll_frame(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(frame) => frame,
            };
            let Some(decoder) = self.gzip.as_mut() else {
                return Poll::Ready(frame.map(|frame| frame.map_err(body_error)));
            };
            match frame {
                None => {
                    let finished = decoder
                        .try_finish()
                        .map(|()| std::mem::take(decoder.get_mut()));
                    self.gzip = None;
                    return Poll::Ready(match finished {
                        Ok(rest) if rest.is_empty() => None,
                        Ok(rest) => Some(Ok(Frame::data(Bytes::from(rest)))),
                        Err(error) => {
                            Some(Err(Error::body(format!("gzip: {error}")).with_source(error)))
                        }
                    });
                }
                Some(Err(error)) => return Poll::Ready(Some(Err(body_error(error)))),
                Some(Ok(frame)) => {
                    let Ok(data) = frame.into_data() else {
                        continue;
                    };
                    if let Err(error) = decoder.write_all(&data) {
                        return Poll::Ready(Some(Err(
                            Error::body(format!("gzip: {error}")).with_source(error)
                        )));
                    }
                    let output = std::mem::take(decoder.get_mut());
                    if !output.is_empty() {
                        return Poll::Ready(Some(Ok(Frame::data(Bytes::from(output)))));
                    }
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        #[cfg(feature = "tcp")]
        if self.gzip.is_some() {
            return false;
        }
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        #[cfg(feature = "tcp")]
        if self.gzip.is_some() {
            return SizeHint::default();
        }
        self.inner.size_hint()
    }
}

fn body_error(error: hyper::Error) -> Error {
    Error::body(http_error_text(&error)).with_source(error)
}

/// moby's `ensureReaderClosed`: up to 512 bytes of the body read and dropped, so a short body
/// (the ping's `OK`, a write's answer) leaves the connection reusable, as it does in Go.
pub async fn ensure_reader_closed(response: Response<Body>) {
    let _ = Limited::new(response.into_body(), DRAIN_LIMIT)
        .collect()
        .await;
}

impl Client {
    /// A client for `settings`, with no connection open yet.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        #[cfg(feature = "tls")]
        let connector = Connector::new(settings.endpoint.clone(), settings.tls.clone());
        #[cfg(not(feature = "tls"))]
        let connector = Connector::new(settings.endpoint.clone());
        let host_header = match &settings.endpoint {
            Endpoint::Unix(_) => DUMMY_HOST.to_owned(),
            #[cfg(feature = "tcp")]
            Endpoint::Tcp(addr) => remove_zone(addr),
        };
        #[cfg(feature = "tls")]
        let tls = settings.tls.is_some();
        #[cfg(not(feature = "tls"))]
        let tls = false;
        let negotiation = Negotiation {
            state: tokio::sync::Mutex::new(NegotiationState {
                version: settings.version.clone(),
                negotiated: settings.pinned,
            }),
            pings: AtomicU64::new(0),
        };
        Self {
            pool: Pool::new(connector, settings.response_header_timeout),
            scheme: if tls { "https" } else { "http" },
            host_header: HeaderValue::from_str(&host_header)
                .unwrap_or_else(|_| HeaderValue::from_static(DUMMY_HOST)),
            user_agent: settings.user_agent_header.clone(),
            // go-connections leaves compression on for every protocol but unix sockets.
            #[cfg(feature = "tcp")]
            gzip: !matches!(settings.endpoint, Endpoint::Unix(_)),
            negotiation,
            settings,
        }
    }

    /// The settings the client was built from.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The API version requests use now: the pinned or negotiated one, else [`MAX_API_VERSION`].
    pub async fn client_version(&self) -> String {
        self.negotiation.state.lock().await.version.clone()
    }

    /// The one request builder: `GET` with Go's headers in Go's order (`Host`, `User-Agent`,
    /// `Accept`, then on TCP the transport's `Accept-Encoding: gzip`) and an empty body. Only
    /// [`Client::ping`] and a write's shape change the method.
    fn request(&self, target: &str, accept: &[&'static str]) -> Request<RequestBody> {
        let mut request = Request::new(Full::new(Bytes::new()));
        *request.method_mut() = Method::GET;
        *request.uri_mut() = target
            .parse()
            .unwrap_or_else(|_| hyper::Uri::from_static("/"));
        let headers = request.headers_mut();
        headers.insert(HOST, self.host_header.clone());
        headers.insert(USER_AGENT, self.user_agent.clone());
        for value in accept {
            headers.append(ACCEPT, HeaderValue::from_static(value));
        }
        #[cfg(feature = "tcp")]
        if self.gzip {
            headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        }
        request
    }

    /// The URL moby's errors name: the address (a socket path escaped) as the host.
    fn url(&self, target: &str) -> String {
        format!(
            "{}://{}{target}",
            self.scheme,
            escape_host(&self.settings.addr)
        )
    }

    /// Sends the request `build` makes (again, if a reused connection fails first and net/http
    /// would send it again), mapping a failure to moby's wording, and sets up the body's
    /// decompression when Go's transport asked for gzip itself. Under a caller's context
    /// ([`crate::with_context`]) a request the context cuts short fails as net/http fails it.
    async fn send(
        &self,
        target: &str,
        build: impl Fn() -> Request<RequestBody>,
    ) -> Result<Response<Body>, Error> {
        let probe = build();
        // net/http's url.Error names the method this way: "Get", "Head".
        let method = probe.method().as_str();
        let operation = format!("{}{}", &method[..1], method[1..].to_ascii_lowercase());
        #[cfg(feature = "tcp")]
        let asked_gzip = probe.headers().contains_key(ACCEPT_ENCODING);
        drop(probe);
        let sent = async {
            self.pool.send(build).await.map_err(|failure| {
                self.connection_error(&operation, &self.url(target), &failure)
                    .with_source(failure)
            })
        };
        let response = match context::current() {
            Some(call) => tokio::select! {
                biased;
                done = call.done() => {
                    return Err(Error::context_request(done, &operation, &self.url(target)));
                }
                response = sent => response?,
            },
            None => sent.await?,
        };
        let (parts, inner) = response.into_parts();
        #[cfg(feature = "tcp")]
        let (parts, gzip) = {
            let mut parts = parts;
            let gzipped = asked_gzip
                && parts
                    .headers
                    .get(CONTENT_ENCODING)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.eq_ignore_ascii_case("gzip"));
            if gzipped {
                parts.headers.remove(CONTENT_ENCODING);
                parts.headers.remove(CONTENT_LENGTH);
            }
            (parts, gzipped.then(|| GzDecoder::new(Vec::new())))
        };
        let body = Body {
            inner,
            #[cfg(feature = "tcp")]
            gzip,
        };
        Ok(Response::from_parts(parts, body))
    }

    /// moby's `ping`: `HEAD /_ping`, used as is on a 200; on a failure or any other status,
    /// `GET /_ping`, whose failure fails the ping and whose status of 400 or above is moby's
    /// response error. Returns the `Api-Version` header, empty when absent. Each answer's body
    /// is drained as moby's `ensureReaderClosed` drains it (up to 512 bytes), so its connection
    /// is reused. With a [`Settings::response_header_timeout`], each drain, and the read of a
    /// `GET /_ping` error body, is bounded by it too: a body that stalls is dropped with its
    /// connection when the bound passes (an error body's ping then fails), so a stalled ping
    /// holds the negotiation (which every request waits on) no longer than its headers could.
    /// Without one, as moby's default, the reads wait as moby's do and the caller bounds the
    /// call. It is moby's `Ping` without `NegotiateAPIVersion`, and the first step of the
    /// negotiation.
    ///
    /// # Errors
    ///
    /// [`Error`] when `GET /_ping` gets no response or a status of 400 or above.
    pub async fn ping(&self) -> Result<String, Error> {
        let target = escape_path(&join_path(&[&self.settings.base_path, "/_ping"]));
        // The only HEAD the client sends; Go's transport asks no gzip for a HEAD.
        let head = || {
            let mut request = self.request(&target, &[]);
            *request.method_mut() = Method::HEAD;
            #[cfg(feature = "tcp")]
            request.headers_mut().remove(ACCEPT_ENCODING);
            request
        };
        if let Ok(response) = self.send(&target, head).await {
            let status = response.status();
            let api_version = api_version_header(&response);
            self.drain_ping(response).await;
            if status == StatusCode::OK {
                return Ok(api_version);
            }
        }
        let response = self.send(&target, || self.request(&target, &[])).await?;
        let api_version = api_version_header(&response);
        if (200..400).contains(&response.status().as_u16()) {
            self.drain_ping(response).await;
            return Ok(api_version);
        }
        let url = self.url(&target);
        match self.settings.response_header_timeout {
            // The error body is read under the same bound as a drain (see above): a body that
            // stalls fails the ping, which fails anyway, instead of holding the negotiation.
            Some(limit) => {
                let status = response.status();
                let status_message = go_status(&response);
                match tokio::time::timeout(limit, self.check_response(response, "GET", &url)).await
                {
                    Ok(checked) => checked?,
                    Err(_) => {
                        return Err(Error::response(
                            status,
                            format!(
                                "request returned {status_message} for API route and version \
                                 {url}; its body did not arrive within {limit:?}"
                            ),
                        ));
                    }
                }
            }
            None => self.check_response(response, "GET", &url).await?,
        }
        Ok(api_version)
    }

    /// [`ensure_reader_closed`] for a ping's answer, bounded by the response-header timeout when
    /// there is one (see [`Client::ping`]).
    async fn drain_ping(&self, response: Response<Body>) {
        match self.settings.response_header_timeout {
            Some(limit) => {
                // On expiry the response, and with it the connection, is dropped.
                let _ = tokio::time::timeout(limit, ensure_reader_closed(response)).await;
            }
            None => ensure_reader_closed(response).await,
        }
    }

    /// moby's `checkVersion`: negotiates once, and again on the next request after a failure,
    /// which moby discards (`_ = cli.checkVersion(ctx)`): the request then goes out at the
    /// current version, [`MAX_API_VERSION`] until a negotiation succeeds. Concurrent requests
    /// share one ping and its outcome, where moby sends one each.
    async fn api_version(&self) -> String {
        let pings = self.negotiation.pings.load(Ordering::Acquire);
        let mut state = self.negotiation.state.lock().await;
        if state.negotiated || self.negotiation.pings.load(Ordering::Acquire) != pings {
            return state.version.clone();
        }
        if let Ok(ping_version) = self.ping().await
            && let Ok(negotiated) = version::negotiate(&ping_version)
        {
            state.version = negotiated;
            state.negotiated = true;
        }
        self.negotiation.pings.fetch_add(1, Ordering::Release);
        state.version.clone()
    }

    /// The target of a versioned request: the negotiated version's path under the host's base
    /// path, escaped, and the query.
    async fn versioned_target(&self, path: &str, query: &str) -> String {
        let version = self.api_version().await;
        let api_path = join_path(&[
            &self.settings.base_path,
            &format!("/v{}", version.strip_prefix('v').unwrap_or(&version)),
            path,
        ]);
        let mut target = escape_path(&api_path);
        if !query.is_empty() {
            target.push('?');
            target.push_str(query);
        }
        target
    }

    /// A versioned `GET` of `path` (with `query`, already encoded, and `accept` header lines):
    /// the response once its headers are in, or moby's error for a status of 400 or above.
    ///
    /// # Errors
    ///
    /// [`Error`] for no response, as moby words it, or a response error.
    pub async fn get(
        &self,
        path: &str,
        query: &str,
        accept: &[&'static str],
    ) -> Result<Response<Body>, Error> {
        let target = self.versioned_target(path, query).await;
        let response = self.send(&target, || self.request(&target, accept)).await?;
        self.checked(response, "GET", &target).await
    }

    /// The response as is for a status below 400, else moby's error for it.
    async fn checked(
        &self,
        response: Response<Body>,
        method: &str,
        target: &str,
    ) -> Result<Response<Body>, Error> {
        if response.status().as_u16() < 400 {
            return Ok(response);
        }
        let url = self.url(target);
        Err(self
            .check_response(response, method, &url)
            .await
            .err()
            .unwrap_or_else(|| Error::response(StatusCode::INTERNAL_SERVER_ERROR, String::new())))
    }

    /// The whole body of a versioned `GET`, read to its end.
    ///
    /// # Errors
    ///
    /// As [`Client::get`], or the body's failure.
    pub async fn get_body(&self, path: &str, query: &str) -> Result<Vec<u8>, Error> {
        let response = self.get(path, query, &[]).await?;
        Ok(response.into_body().collect().await?.to_bytes().to_vec())
    }

    /// A list call (`GET /containers/json`, …) decoded as Go decodes a slice
    /// ([`decode_list`]).
    ///
    /// # Errors
    ///
    /// As [`Client::get_body`], or the decode error.
    pub async fn get_list<T>(&self, path: &str, query: &str) -> Result<Vec<T>, Error>
    where
        T: Default + for<'de> Deserialize<'de>,
    {
        decode_list(&self.get_body(path, query).await?)
    }

    /// An inspect call (`GET /containers/{id}/json`, …) decoded as Go's decoder decodes its first
    /// value ([`decode_first`]).
    ///
    /// # Errors
    ///
    /// As [`Client::get_body`], or the decode error.
    pub async fn get_object<T>(&self, path: &str) -> Result<T, Error>
    where
        T: Default + for<'de> Deserialize<'de>,
    {
        decode_first(&self.get_body(path, "").await?)
    }

    /// moby's `Events`: `GET /events` with `query` (already encoded, see
    /// [`crate::encode_query`], [`crate::since_parameter`] and [`crate::Filters`]) and moby's
    /// `Accept` lines; resolves once the response headers passed moby's status check, with the
    /// stream decoded as the response's `Content-Type` says.
    ///
    /// # Errors
    ///
    /// As [`Client::get`].
    pub async fn events(&self, query: &str) -> Result<JsonStream<Body>, Error> {
        let response = self.get("/events", query, &EVENTS_ACCEPT).await?;
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(JsonStream::new(
            response.into_body(),
            content_type.as_deref(),
        ))
    }

    /// A versioned request of `path` and `query` from the one builder, which `shape` turns into
    /// the call (its method, headers and body), sent on the shared pool. A request that is not
    /// a `GET` or `HEAD` is sent again on another connection only if it was never written. The
    /// response once its headers are in, or moby's error for a status of 400 or above.
    ///
    /// net/http sends `Content-Length: 0` with an empty `POST` and hyper sends none, so `shape`
    /// sets it where Go's request has it.
    ///
    /// # Errors
    ///
    /// [`Error`] for no response, as moby words it, or a response error.
    #[cfg(feature = "write")]
    pub async fn exchange(
        &self,
        path: &str,
        query: &str,
        shape: impl Fn(&mut Request<Full<Bytes>>),
    ) -> Result<Response<Body>, Error> {
        let target = self.versioned_target(path, query).await;
        let build = || {
            let mut request = self.request(&target, &[]);
            shape(&mut request);
            #[cfg(feature = "tcp")]
            if request.method() == Method::HEAD {
                request.headers_mut().remove(ACCEPT_ENCODING);
            }
            request
        };
        let method = build().method().as_str().to_owned();
        let response = self.send(&target, build).await?;
        self.checked(response, &method, &target).await
    }

    /// [`Client::exchange`] on a connection of its own that the answer upgrades, as moby's
    /// `postHijacked` sends the attach: the upgraded connection on a `101`, and moby's
    /// `setupHijackConn` errors otherwise. The exchange is outside any caller's context, as moby's
    /// hijacked round trip is.
    ///
    /// # Errors
    ///
    /// [`Error`] when the daemon cannot be dialled, the exchange fails, or the answer is not an
    /// upgrade.
    #[cfg(feature = "attach")]
    pub async fn exchange_upgrade(
        &self,
        path: &str,
        query: &str,
        shape: impl Fn(&mut Request<Full<Bytes>>),
    ) -> Result<hyper::upgrade::Upgraded, Error> {
        let target = self.versioned_target(path, query).await;
        let mut request = self.request(&target, &[]);
        // moby writes the hijacked request on the connection itself (`req.Write`), never through
        // the transport, so no `Accept-Encoding: gzip` goes out with it, on TCP either. It is the
        // builder's last header, so removing it before `shape` leaves the order of the rest as it
        // is on a unix socket (a removal swaps the last entry into the freed slot).
        #[cfg(feature = "tcp")]
        request.headers_mut().remove(ACCEPT_ENCODING);
        shape(&mut request);
        let response = self
            .pool
            .send_upgradable(request)
            .await
            .map_err(|failure| {
                let error = match &failure {
                    SendError::Connect(error) => Error::connection_failed(format!(
                        "cannot connect to the Docker daemon. Is 'docker daemon' running on this \
                         host?: {error}"
                    )),
                    SendError::Http(error) => Error::connection_failed(http_error_text(error)),
                    SendError::HeaderTimeout => Error::connection_failed(failure.to_string()),
                };
                error.with_source(failure)
            })?;
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            return Err(Error::connection_failed(format!(
                "unable to upgrade to tcp, received {}",
                response.status().as_u16()
            )));
        }
        hyper::upgrade::on(response)
            .await
            .map_err(|error| Error::connection_failed(http_error_text(&error)).with_source(error))
    }

    /// moby's `checkResponseErr`: a status from 200 to 399 is fine; otherwise the error carries
    /// the daemon's message (the JSON `message` for `application/json`, else the body), or says
    /// it is missing or too large.
    async fn check_response(
        &self,
        response: Response<Body>,
        method: &str,
        url: &str,
    ) -> Result<(), Error> {
        let status = response.status();
        if (200..400).contains(&status.as_u16()) {
            return Ok(());
        }
        let status_message = go_status(&response);
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let too_large = || {
            Error::response(
                status,
                format!(
                    "request returned {status_message} with a message (> {ERROR_BODY_LIMIT} \
                     bytes) for API route and version {url}, check if the server supports the \
                     requested API version"
                ),
            )
        };
        let mut body = BytesMut::new();
        if method != "HEAD" {
            let mut limited = Limited::new(response.into_body(), ERROR_BODY_LIMIT);
            while let Some(frame) = limited.frame().await {
                match frame {
                    Ok(frame) => {
                        if let Ok(data) = frame.into_data() {
                            body.extend_from_slice(&data);
                        }
                    }
                    Err(error) if error.is::<http_body_util::LengthLimitError>() => {
                        return Err(too_large().with_source(error));
                    }
                    Err(error) => {
                        return Err(Error::response(status, error.to_string()).with_source(error));
                    }
                }
            }
        }
        if body.len() >= ERROR_BODY_LIMIT {
            return Err(too_large());
        }
        if body.is_empty() {
            return Err(Error::response(
                status,
                format!(
                    "request returned {status_message} for API route and version {url}, check if \
                     the server supports the requested API version"
                ),
            ));
        }
        let daemon_message = if content_type.as_deref() == Some("application/json") {
            match serde_json::from_slice::<ErrorResponse>(&body) {
                Err(error) => {
                    return Err(
                        Error::response(status, format!("error reading JSON: {error}"))
                            .with_source(error),
                    );
                }
                Ok(ErrorResponse { message })
                    if message.as_deref().unwrap_or_default().is_empty() =>
                {
                    format!(
                        "API returned a {} ({}) but provided no error-message",
                        status.as_u16(),
                        status_text(status.as_u16())
                    )
                }
                Ok(ErrorResponse { message }) => message.unwrap_or_default().trim().to_owned(),
            }
        } else {
            String::from_utf8_lossy(&body).trim().to_owned()
        };
        Err(Error::response(
            status,
            format!("Error response from daemon: {daemon_message}"),
        ))
    }

    /// moby's `doRequest` wording of a request that got no response.
    fn connection_error(&self, operation: &str, url: &str, failure: &SendError) -> Error {
        let host = self.settings.daemon_host();
        let inner = match failure {
            SendError::Connect(error) => error.to_string(),
            SendError::HeaderTimeout => failure.to_string(),
            SendError::Http(error) => http_error_text(error),
        };
        let url_error = format!("{operation} {}: {inner}", quote(url));
        if self.scheme != "https" && matches!(failure, SendError::Http(error) if error.is_parse()) {
            return Error::connection_failed(format!(
                "{url_error}.\n* Are you trying to connect to a TLS-enabled daemon without TLS?"
            ));
        }
        if let SendError::Connect(error) = failure {
            #[cfg(feature = "tls")]
            if self.scheme == "https"
                && matches!(
                    error.tls_error(),
                    Some(rustls::Error::AlertReceived(
                        rustls::AlertDescription::HandshakeFailure
                            | rustls::AlertDescription::BadCertificate
                    ))
                )
            {
                return Error::connection_failed(format!(
                    "the server probably has client authentication (--tlsverify) enabled; check \
                     your TLS client certification settings: {url_error}"
                ));
            }
            if let Some(dial) = error.dial_error() {
                match dial.raw_os_error() {
                    Some(1 | 13) => {
                        return Error::connection_failed(format!(
                            "permission denied while trying to connect to the docker API at {host}"
                        ));
                    }
                    Some(2) => {
                        return Error::connection_failed(format!(
                            "failed to connect to the docker API at {host}; check if the path is \
                             correct and if the daemon is running: {inner}"
                        ));
                    }
                    _ => {}
                }
            }
            #[cfg(feature = "tcp")]
            if error.is_lookup() {
                let lookup = inner.strip_prefix("dial tcp: ").unwrap_or(&inner);
                return Error::connection_failed(format!(
                    "failed to connect to the docker API at {host}: {lookup}"
                ));
            }
        }
        let timeout = match failure {
            SendError::Connect(error) => error.is_timeout(),
            SendError::HeaderTimeout => true,
            SendError::Http(_) => false,
        };
        if timeout || inner.contains("connection refused") || inner.contains("dial unix") {
            return Error::connection_failed(format!(
                "Cannot connect to the Docker daemon at {host}. Is the docker daemon running?"
            ));
        }
        Error::connection_failed(format!("error during connect: {url_error}"))
    }
}

/// The text of a failed exchange, as close to Go's as hyper allows: a connection closed before a
/// response is Go's `EOF`, a malformed response is net/http's, and an I/O error is its errno text.
fn http_error_text(error: &hyper::Error) -> String {
    if error.is_parse() {
        return "net/http: HTTP/1.x transport connection broken: malformed HTTP response"
            .to_owned();
    }
    if error.is_incomplete_message() || error.is_closed() || error.is_canceled() {
        return "EOF".to_owned();
    }
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_error_text(io);
        }
        source = cause.source();
    }
    error.to_string()
}

/// The `Api-Version` header, as Go's `Header.Get` reads it: the first value, empty when absent.
fn api_version_header<B>(response: &Response<B>) -> String {
    response
        .headers()
        .get("Api-Version")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Go's `resp.Status`: the code and the reason from the wire, which hyper keeps only when it is
/// not the canonical one.
fn go_status<B>(response: &Response<B>) -> String {
    let code = response.status().as_u16();
    let reason = response
        .extensions()
        .get::<hyper::ext::ReasonPhrase>()
        .map(|reason| String::from_utf8_lossy(reason.as_bytes()).into_owned())
        .or_else(|| response.status().canonical_reason().map(str::to_owned))
        .unwrap_or_default();
    if reason.is_empty() {
        code.to_string()
    } else {
        format!("{code} {reason}")
    }
}

/// moby's `common.ErrorResponse`.
#[derive(Debug, Deserialize)]
struct ErrorResponse {
    // A missing or null message is Go's empty string.
    #[serde(default)]
    message: Option<String>,
}
