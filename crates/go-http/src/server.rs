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

//! Go's `http.ServeMux` behind an `http.Server`, on hyper: the routing, Go's answers, header
//! order and framing, one connection's service, and the accept loop with its drain.
//!
//! Extracted from swarm-device-access/src/server.rs at 8b4ab3a3e417 (`Reply::sniffed`, `serve`
//! with `on_error`) and swarm-scheduler-exporter/src/server.rs at c9e79afa8810 (the same service,
//! framing and drain), with each program's routes, handlers, logger and metrics registry taken
//! out: the caller supplies them as a [`Mux`] and a log callback.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use go_clock::Clock;
use go_compat::duration;
use go_compat::errno::io_error_text;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::header::{
    ACCEPT, ACCEPT_ENCODING, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, DATE,
    HeaderName, HeaderValue, LOCATION,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Version};
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::{GracefulConnection, GracefulShutdown};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::time::Instant;

use crate::gopath::{self, Route};
use crate::timeouts::{ConnState, TimedIo, Timeouts};

/// The type Go's `http.Error` sets, and the one it sniffs for a text body.
pub const TEXT_PLAIN: &str = "text/plain; charset=utf-8";
const TEXT_HTML: &str = "text/html; charset=utf-8";

/// Go's `bufferBeforeChunkingSize`: a handler that writes at most this much gets a
/// `Content-Length`; anything longer is chunked.
const BUFFER_BEFORE_CHUNKING: usize = 2048;

/// An answer before Go's header order and framing are applied: what a handler wrote.
#[derive(Debug, Clone)]
pub struct Reply {
    status: StatusCode,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Bytes,
    // OPTIONS * sets its Content-Length itself.
    explicit_length: bool,
    // A type the handler did not set, which Go sniffs from the body and writes after
    // Content-Length.
    sniffed_type: Option<HeaderValue>,
}

impl Reply {
    /// A handler that set `headers` and wrote `body` with `status`. A `Content-Length` among the
    /// headers is dropped: Go sets its own.
    #[must_use]
    pub fn new(
        status: StatusCode,
        headers: Vec<(HeaderName, HeaderValue)>,
        body: impl Into<Bytes>,
    ) -> Self {
        Self {
            status,
            headers,
            body: body.into(),
            explicit_length: false,
            sniffed_type: None,
        }
    }

    /// A handler that wrote the text `body` without setting a type: Go sniffs `text/plain`
    /// ([`TEXT_PLAIN`]) and writes it after `Content-Length`.
    #[must_use]
    pub fn sniffed(status: StatusCode, body: &'static str) -> Self {
        Self {
            sniffed_type: Some(HeaderValue::from_static(TEXT_PLAIN)),
            ..Self::new(status, Vec::new(), body)
        }
    }

    /// promhttp's answer: a 200 with the exposition's `Content-Encoding`, when compressed, and its
    /// `Content-Type`; a type that is not a valid header value is left out.
    #[must_use]
    pub fn metrics(
        content_type: &str,
        content_encoding: Option<&'static str>,
        body: impl Into<Bytes>,
    ) -> Self {
        let mut headers = Vec::with_capacity(2);
        if let Some(encoding) = content_encoding {
            headers.push((CONTENT_ENCODING, HeaderValue::from_static(encoding)));
        }
        if let Ok(content_type) = HeaderValue::from_str(content_type) {
            headers.push((CONTENT_TYPE, content_type));
        }
        Self::new(StatusCode::OK, headers, body)
    }
}

/// `http.Error(w, "404 page not found", 404)`.
fn not_found() -> Reply {
    Reply::new(
        StatusCode::NOT_FOUND,
        vec![
            (CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN)),
            (
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            ),
        ],
        "404 page not found\n",
    )
}

/// `http.Redirect(w, r, location, 307)`. Go compares the method case-sensitively: only `GET`
/// gets the body, and only `GET` and `HEAD` the `Content-Type`.
fn redirect(method: &Method, location: &str) -> Reply {
    let Ok(location_value) = HeaderValue::from_str(location) else {
        // The location is escaped, so always a valid header value.
        return not_found();
    };
    let mut headers = Vec::with_capacity(2);
    let is_get = method.as_str() == "GET";
    if is_get || method.as_str() == "HEAD" {
        headers.push((CONTENT_TYPE, HeaderValue::from_static(TEXT_HTML)));
    }
    headers.push((LOCATION, location_value));
    let body = if is_get {
        format!(
            "<a href=\"{}\">Temporary Redirect</a>.\n\n",
            gopath::html_escape(location)
        )
    } else {
        String::new()
    };
    Reply::new(StatusCode::TEMPORARY_REDIRECT, headers, body)
}

/// What promhttp reads from a scrape: the first `Accept` header, every `Accept-Encoding` one,
/// and the `name[]` query filter, none when the query names no family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsRequest {
    /// The first `Accept` header.
    pub accept: Option<String>,
    /// Every `Accept-Encoding` header.
    pub accept_encoding: Vec<String>,
    /// The `name[]` values; a value that is not UTF-8 names no family.
    pub names: Option<BTreeSet<String>>,
}

impl MetricsRequest {
    /// What promhttp reads from `request`.
    #[must_use]
    pub fn of<B>(request: &Request<B>) -> Self {
        let header_text =
            |value: &HeaderValue| String::from_utf8_lossy(value.as_bytes()).into_owned();
        let names = request.uri().query().and_then(|query| {
            gopath::parse_query(query)
                .remove(b"name[]".as_slice())
                .map(|names| {
                    names
                        .into_iter()
                        .filter_map(|name| String::from_utf8(name).ok())
                        .collect()
                })
        });
        Self {
            accept: request.headers().get(ACCEPT).map(header_text),
            accept_encoding: request
                .headers()
                .get_all(ACCEPT_ENCODING)
                .iter()
                .map(header_text)
                .collect(),
            names,
        }
    }
}

/// A handler's answer, once it is ready.
pub type ReplyFuture = Pin<Box<dyn Future<Output = Reply> + Send>>;

/// A route's handler: the request's head (the body is not read, as the Go handlers did not read
/// it) to its answer.
pub type Handler = Arc<dyn Fn(Request<()>) -> ReplyFuture + Send + Sync>;

/// The routes, Go's `http.ServeMux` with exact one-segment patterns (`/metrics`) and any method.
#[derive(Clone, Default)]
pub struct Mux {
    patterns: Vec<&'static str>,
    handlers: Vec<Handler>,
}

impl fmt::Debug for Mux {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Mux")
            .field("patterns", &self.patterns)
            .finish_non_exhaustive()
    }
}

impl Mux {
    /// No routes: every path is Go's 404, or a 307 to its cleaned form.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `pattern`, an exact path of one segment such as `/healthz`, with `handler`.
    #[must_use]
    pub fn route(
        self,
        pattern: &'static str,
        handler: impl Fn(&Request<()>) -> Reply + Send + Sync + 'static,
    ) -> Self {
        self.route_async(pattern, move |request: Request<()>| {
            std::future::ready(handler(&request))
        })
    }

    /// Serves `pattern` with a handler that answers later.
    #[must_use]
    pub fn route_async<F, Fut>(mut self, pattern: &'static str, handler: F) -> Self
    where
        F: Fn(Request<()>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send + 'static,
    {
        self.patterns.push(pattern);
        self.handlers
            .push(Arc::new(move |request| Box::pin(handler(request))));
        self
    }

    /// The patterns served, in the order they were added.
    #[must_use]
    pub fn patterns(&self) -> &[&'static str] {
        &self.patterns
    }

    /// Routes one request as Go's mux does, and answers it.
    async fn handle(&self, request: Request<()>) -> Reply {
        let method = request.method().clone();
        // serverHandler.ServeHTTP hands `OPTIONS *` to the global options handler.
        if method == Method::OPTIONS && request.uri().path() == "*" {
            let mut reply = Reply::new(
                StatusCode::OK,
                vec![(CONTENT_LENGTH, HeaderValue::from_static("0"))],
                Bytes::new(),
            );
            reply.explicit_length = true;
            return reply;
        }
        // CONNECT's target is an authority, which matches no path pattern.
        if method == Method::CONNECT {
            return not_found();
        }
        match gopath::route(request.uri().path(), request.uri().query(), &self.patterns) {
            Route::Matched(pattern) => {
                match self.patterns.iter().position(|known| *known == pattern) {
                    Some(index) => (self.handlers[index])(request).await,
                    None => not_found(),
                }
            }
            Route::Redirect(location) => redirect(&method, &location),
            Route::NotFound => not_found(),
            Route::BadRequest => Reply::new(
                StatusCode::BAD_REQUEST,
                vec![
                    (CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN)),
                    (CONNECTION, HeaderValue::from_static("close")),
                ],
                "400 Bad Request",
            ),
        }
    }
}

/// The server's own messages: Go's `net/http` logs them with `log.Printf` (an accept error it
/// retries). The callback receives the message alone; [`log_printf_line`] gives it Go's prefix.
pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// What a server answers with: its routes, the clock `Date` is read from, and where its own
/// messages go.
pub struct Server {
    mux: Mux,
    clock: Arc<dyn Clock>,
    log: LogFn,
}

impl fmt::Debug for Server {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Server")
            .field("mux", &self.mux)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Serves `mux`, with `Date` read from `clock` and the server's messages handed to `log`.
    #[must_use]
    pub fn new(
        mux: Mux,
        clock: Arc<dyn Clock>,
        log: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            mux,
            clock,
            log: Arc::new(log),
        }
    }

    /// The routes.
    #[must_use]
    pub fn mux(&self) -> &Mux {
        &self.mux
    }
}

/// Go's `log.Printf` line for `message` at `now` in `zone`: `2006/01/02 15:04:05 <message>` and
/// a newline, as the standard logger writes it to stderr.
#[must_use]
pub fn log_printf_line(now: Timestamp, zone: &TimeZone, message: &str) -> String {
    let civil = zone.to_offset(now).to_datetime(now);
    format!(
        "{:04}/{:02}/{:02} {:02}:{:02}:{:02} {message}\n",
        civil.year(),
        civil.month(),
        civil.day(),
        civil.hour(),
        civil.minute(),
        civil.second()
    )
}

/// Whether an HTTP/1.1 request asked for the connection to close, as Go's `wantsClose` reads its
/// `Connection` tokens.
fn wants_close<B>(request: &Request<B>) -> bool {
    request.version() == Version::HTTP_11
        && request.headers().get_all(CONNECTION).iter().any(|value| {
            String::from_utf8_lossy(value.as_bytes())
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        })
}

/// Go's `http.TimeFormat` of `now`.
fn http_date(clock: &dyn Clock) -> HeaderValue {
    let date = clock
        .now()
        .strftime("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    HeaderValue::from_str(&date).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// The answer in Go's header order and framing, with a body that reports to `state` when hyper
/// is done with it, to a request that was a `HEAD` (`is_head`) and asked to close the connection
/// (`wants_close`).
fn finish(
    reply: Reply,
    is_head: bool,
    wants_close: bool,
    date: HeaderValue,
    state: ConnState,
) -> Response<ResponseBody> {
    let Reply {
        status,
        mut headers,
        body,
        explicit_length,
        sniffed_type,
    } = reply;
    // The handler's headers sorted by name, as Go writes them; a sort on the lowercase names
    // gives the same order as one on Go's canonical ones for header-name characters.
    headers.retain(|(name, _)| explicit_length || *name != CONTENT_LENGTH);
    headers.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    let connection = headers
        .iter()
        .position(|(name, _)| *name == CONNECTION)
        .map(|index| headers.remove(index));
    let chunked = body.len() > BUFFER_BEFORE_CHUNKING;
    let mut response = Response::new(ResponseBody {
        data: body.clone(),
        chunked,
        state: Some(state),
    });
    *response.status_mut() = status;
    let map = response.headers_mut();
    for (name, value) in headers {
        map.append(name, value);
    }
    map.insert(DATE, date);
    // Go sets the length of a body it buffered whole, except for a HEAD answer that wrote
    // nothing.
    let head_wrote_nothing = is_head && body.is_empty();
    if !(explicit_length || chunked || head_wrote_nothing) {
        map.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
    }
    if let Some(sniffed) = sniffed_type {
        map.insert(CONTENT_TYPE, sniffed);
    }
    if let Some((name, value)) = connection {
        map.insert(name, value);
    } else if wants_close {
        map.insert(CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

/// A response body that tells the connection's deadlines when hyper is done with it, and that
/// never reports having ended: hyper 1.12 otherwise treats it as absent, drops a `Content-Length:
/// 0` set by hand and appends its own after every other header.
#[derive(Debug)]
pub struct ResponseBody {
    data: Bytes,
    chunked: bool,
    state: Option<ConnState>,
}

impl Drop for ResponseBody {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            state.on_response_done();
        }
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if self.data.is_empty() {
            return Poll::Ready(None);
        }
        let size = if self.chunked {
            self.data.len().min(BUFFER_BEFORE_CHUNKING)
        } else {
            self.data.len()
        };
        let frame = self.data.split_to(size);
        Poll::Ready(Some(Ok(Frame::data(frame))))
    }

    fn is_end_stream(&self) -> bool {
        false
    }

    fn size_hint(&self) -> SizeHint {
        if self.chunked {
            SizeHint::default()
        } else {
            SizeHint::with_exact(u64::try_from(self.data.len()).unwrap_or(u64::MAX))
        }
    }
}

/// One HTTP/1 connection over `io`, with Go's deadlines, header case and order.
pub fn connection<I>(
    io: I,
    server: Arc<Server>,
    timeouts: Timeouts,
) -> impl GracefulConnection<Error = hyper::Error> + Send + 'static
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let state = ConnState::new(timeouts);
    let io = TokioIo::new(TimedIo::new(io, state.clone()));
    let service = service_fn(move |request: Request<Incoming>| {
        let server = Arc::clone(&server);
        let state = state.clone();
        state.on_request_head();
        async move {
            // The handler gets the head; the body stays alive, unread, until the answer is made,
            // as the Go handlers left it.
            let (parts, body) = request.into_parts();
            let head = Request::from_parts(parts, ());
            let is_head = head.method() == Method::HEAD;
            let close = wants_close(&head);
            let reply = server.mux.handle(head).await;
            let date = http_date(&*server.clock);
            let response = finish(reply, is_head, close, date, state);
            drop(body);
            Ok::<_, Infallible>(response)
        }
    });
    http1::Builder::new()
        .title_case_headers(true)
        .auto_date_header(false)
        // No hyper timer: the deadlines are the I/O wrapper's.
        .header_read_timeout(None)
        .serve_connection(io, service)
}

/// How the drain ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drained {
    /// Every connection finished before the deadline.
    Clean,
    /// Connections were still open at the deadline, Go's `Shutdown` returning the context's
    /// error.
    DeadlineExceeded,
}

/// How [`serve`] ended: the drain and the deadline it ran to.
#[derive(Debug)]
pub struct Served {
    /// How the drain ended.
    pub drained: Drained,
    /// The drain's deadline.
    pub deadline: Instant,
}

/// Whether Go's `Serve` retries an accept error: `syscall.Errno.Temporary` (`EINTR`, `EMFILE`,
/// `ENFILE`, and the timeouts `EAGAIN` and `ETIMEDOUT`), and `ECONNABORTED`, which Go's poller
/// retries by itself.
fn temporary(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(4 | 11 | 23 | 24 | 103 | 110))
}

/// Serves `server` on `listener` until `shutdown` resolves, then closes the listener at once, so
/// later connects are refused, and waits for the open connections until `drain_deadline()`,
/// computed when the drain starts.
///
/// An accept error Go's `Serve` retries is passed to the server's log callback as Go's message
/// (`http: Accept error: …; retrying in 5ms`), and the accept retried after Go's backoff (5 ms,
/// doubling, at most 1 s). Any other accept error closes the listener, as `Serve` returning
/// does, and is handed to `on_error` at once, worded as Go's `*net.OpError`
/// (`accept tcp <addr>: accept4: <errno text>`); the open connections are then still served until
/// `shutdown`, as Go's are until `Shutdown`.
///
/// To stop at once on such an error instead (Go's `serveUntilDone`, which drains as soon as
/// `Serve` returns), make `shutdown` also resolve when `on_error` runs: for example,
/// `on_error` keeps the error and cancels a token that `shutdown` waits on along with the
/// program's own stop. `shutdown` is polled after `on_error` returns, so the drain then starts
/// at once, and its deadline is computed then.
pub async fn serve(
    listener: TcpListener,
    server: Arc<Server>,
    timeouts: Timeouts,
    shutdown: impl Future<Output = ()>,
    drain_deadline: impl FnOnce() -> Instant,
    on_error: impl FnOnce(io::Error),
) -> Served {
    let graceful = GracefulShutdown::new();
    let local = listener
        .local_addr()
        .map_or_else(|_| String::new(), |address| address.to_string());
    let mut shutdown = std::pin::pin!(shutdown);
    let mut retry = std::time::Duration::ZERO;
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    retry = std::time::Duration::ZERO;
                    let watched =
                        graceful.watch(connection(stream, Arc::clone(&server), timeouts));
                    tokio::spawn(async move {
                        let _ = watched.await;
                    });
                }
                Err(accept_error) if temporary(&accept_error) => {
                    // Go's Serve: 5ms, doubling, at most 1s.
                    retry = if retry.is_zero() {
                        std::time::Duration::from_millis(5)
                    } else {
                        (retry * 2).min(std::time::Duration::from_secs(1))
                    };
                    (server.log)(&format!(
                        "http: Accept error: accept tcp {local}: accept4: {}; retrying in {}",
                        io_error_text(&accept_error),
                        duration::format(i64::try_from(retry.as_nanos()).unwrap_or(i64::MAX))
                    ));
                    tokio::time::sleep(retry).await;
                }
                Err(accept_error) => {
                    drop(listener);
                    on_error(io::Error::new(
                        accept_error.kind(),
                        format!("accept tcp {local}: accept4: {}", io_error_text(&accept_error)),
                    ));
                    shutdown.await;
                    return drain(graceful, drain_deadline()).await;
                }
            },
        }
    }
    drop(listener);
    drain(graceful, drain_deadline()).await
}

/// Waits for the open connections until `deadline`.
async fn drain(graceful: GracefulShutdown, deadline: Instant) -> Served {
    let drained = match tokio::time::timeout_at(deadline, graceful.shutdown()).await {
        Ok(()) => Drained::Clean,
        Err(_) => Drained::DeadlineExceeded,
    };
    Served { drained, deadline }
}

#[cfg(test)]
mod tests;
