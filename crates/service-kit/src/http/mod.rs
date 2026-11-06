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

//! A minimal HTTP/1 server for a service's own endpoints (metrics, health, a WebSocket): an accept
//! loop, one tracked task per connection, and a bounded shutdown.
//!
//! [`serve`] takes a bound [`TcpListener`], a [tower](tower_service) service (an `axum::Router` is
//! one), a [`ServerConfig`] and a [`Lifecycle`]. Per connection it sets `TCP_NODELAY`, bounds the
//! request head (`header_read_timeout`, `max_headers`, `max_header_size`, `max_buf_size`) and
//! closes a connection whose peer stopped reading a response for the write-stall timeout, which
//! also covers a body streamed after the handler returned.
//!
//! Shutdown, when [`Lifecycle::stop`] is called: the listener is dropped, every connection gets
//! hyper's `graceful_shutdown` (an idle one closes, a request in flight finishes and closes), and
//! `serve` waits for the tracker until the [`drain`](ServerConfig::drain) deadline. Past it the
//! `cut` token is cancelled, every connection is dropped, and the outcome is
//! [`Outcome::DeadlineExceeded`]. A request in flight at `stop` is answered with
//! `Connection: close`, which turns an upgrade's `101` into a refused upgrade; a connection
//! upgraded before `stop` is unaffected and ends when its task, spawned with [`Lifecycle::spawn`],
//! does or when `cut` fires.
//!
//! Every request carries a [`Lifecycle`] and a [`Peer`] in its extensions.
//!
//! Extracted from bitgauge/src/server/conn.rs at d841694700a5 (the accept loop with its backoff,
//! the connection driver, the shutdown sequence), simplified: no request-phase tracking, so the
//! idle and request timeouts of bitgauge's `timeouts.rs` give way to hyper's header-read timeout
//! and a write-stall deadline (`stall.rs`).

mod lifecycle;
mod stall;

use std::error::Error as StdError;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use hyper::body::{Body, Incoming};
use hyper::server::conn::http1;
use hyper::{Request, Response};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

pub use lifecycle::Lifecycle;
use stall::StallIo;

/// The error type a service or its body may fail with.
pub type BoxError = Box<dyn StdError + Send + Sync>;

/// How long the tracker is given after `cut`, before `serve` returns anyway.
pub const CUT_WAIT: Duration = Duration::from_secs(1);

/// The first and the longest pause after a failed accept, as net/http's `Serve` backs off.
const ACCEPT_BACKOFF_FIRST: Duration = Duration::from_millis(5);
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

/// The TCP peer of a request's connection, stored in each request's extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer(pub SocketAddr);

/// The server's settings. The three durations are the caller's to choose; the sizes are hyper's
/// own (100 headers, ~400 KiB of buffer) until set.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ServerConfig {
    /// How long a client may take to send a request's head, an idle keep-alive connection's wait
    /// for the next request included; the connection is closed past it.
    pub header_read_timeout: Duration,
    /// How long a write may wait without a single byte leaving before the connection is closed.
    /// The socket becomes writable only once a good part of its send buffer (megabytes, on a
    /// fast link) has drained, so a slow but steady reader looks stalled for as long as that
    /// takes: choose a value well above `send buffer / slowest acceptable rate`.
    pub write_stall_timeout: Duration,
    /// How long [`serve`] waits for the connections to finish after `stop`, before the cut.
    pub drain: Duration,
    /// The most headers a request may carry; hyper answers more with `431`.
    pub max_headers: Option<usize>,
    /// The largest request head in bytes; hyper answers a larger one with `431`.
    pub max_header_size: Option<usize>,
    /// The connection's buffer size (hyper requires at least 8192).
    pub max_buf_size: Option<usize>,
    /// Whether a handler may take over a connection (`hyper::upgrade::on`), for a WebSocket.
    pub upgrades: bool,
}

impl ServerConfig {
    /// Settings with the three deadlines, no upgrades, and hyper's sizes.
    #[must_use]
    pub fn new(
        header_read_timeout: Duration,
        write_stall_timeout: Duration,
        drain: Duration,
    ) -> Self {
        Self {
            header_read_timeout,
            write_stall_timeout,
            drain,
            max_headers: None,
            max_header_size: None,
            max_buf_size: None,
            upgrades: false,
        }
    }

    /// Bounds the number of headers of a request.
    #[must_use]
    pub fn max_headers(mut self, max: usize) -> Self {
        self.max_headers = Some(max);
        self
    }

    /// Bounds the size of a request's head.
    #[must_use]
    pub fn max_header_size(mut self, max: usize) -> Self {
        self.max_header_size = Some(max);
        self
    }

    /// Sets the connection's buffer size, which also bounds a head when no `max_header_size` is
    /// set. Hyper panics on less than 8192.
    #[must_use]
    pub fn max_buf_size(mut self, max: usize) -> Self {
        self.max_buf_size = Some(max);
        self
    }

    /// Lets handlers take over the connection with `hyper::upgrade::on`.
    #[must_use]
    pub fn with_upgrades(mut self) -> Self {
        self.upgrades = true;
        self
    }
}

/// How [`serve`] ended.
#[derive(Debug)]
pub enum Outcome {
    /// Every connection and task finished within the drain deadline.
    Drained,
    /// The drain deadline passed: the remaining connections were cut.
    DeadlineExceeded,
    /// The listener failed with an error that retrying cannot fix; the connections were drained
    /// as for a stop, within the deadline or cut at it.
    ListenerFailed(io::Error),
}

/// Whether an accept error is about this one connection or a passing shortage, so the loop backs
/// off and retries rather than stopping the server.
fn accept_error_is_transient(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::OutOfMemory
    ) {
        return true;
    }
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        return [
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOBUFS,
            libc::ENOMEM,
            libc::EPROTO,
        ]
        .contains(&code);
    }
    false
}

/// Serves `service` on `listener` until [`Lifecycle::stop`], then drains within
/// [`ServerConfig::drain`] and cuts what is left.
///
/// The service is cloned per connection and per request; a failed `poll_ready` or `call` fails
/// that request's connection. Cancel the future by dropping it only after the sequence ended:
/// dropping it earlier leaves the connections to the runtime.
pub async fn serve<S, B>(
    listener: TcpListener,
    service: S,
    config: ServerConfig,
    lifecycle: &Lifecycle,
) -> Outcome
where
    S: tower_service::Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    let failed = accept_loop(listener, &service, config, lifecycle)
        .await
        .err();
    if failed.is_some() {
        lifecycle.stop();
    }
    drain(lifecycle, Instant::now() + config.drain, failed).await
}

/// Accepts until `stop`, spawning one tracked task per connection; the listener is dropped on
/// return. Fails only on an accept error that is not transient.
async fn accept_loop<S, B>(
    listener: TcpListener,
    service: &S,
    config: ServerConfig,
    lifecycle: &Lifecycle,
) -> io::Result<()>
where
    S: tower_service::Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    let mut backoff: Option<Duration> = None;
    loop {
        let accepted = tokio::select! {
            biased;
            () = lifecycle.stop.cancelled() => return Ok(()),
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, peer)) => {
                backoff = None;
                let service = service.clone();
                let connection = lifecycle.clone();
                lifecycle.tracker.spawn(async move {
                    drive(stream, peer, service, config, &connection).await;
                });
            }
            Err(error) if accept_error_is_transient(&error) => {
                let delay = backoff.map_or(ACCEPT_BACKOFF_FIRST, |delay| {
                    (delay * 2).min(ACCEPT_BACKOFF_MAX)
                });
                backoff = Some(delay);
                tokio::select! {
                    biased;
                    () = lifecycle.stop.cancelled() => return Ok(()),
                    () = tokio::time::sleep(delay) => {}
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// The shutdown sequence after the listener is gone, all under one deadline: close the tracker,
/// wait for it until `deadline`, and past it cancel `cut` and give the tracker [`CUT_WAIT`] more.
async fn drain(lifecycle: &Lifecycle, deadline: Instant, failed: Option<io::Error>) -> Outcome {
    lifecycle.tracker.close();
    let finished = tokio::time::timeout_at(deadline, lifecycle.tracker.wait())
        .await
        .is_ok();
    if !finished {
        lifecycle.cut.cancel();
        let _ = tokio::time::timeout(CUT_WAIT, lifecycle.tracker.wait()).await;
    }
    match (failed, finished) {
        (Some(error), _) => Outcome::ListenerFailed(error),
        (None, true) => Outcome::Drained,
        (None, false) => Outcome::DeadlineExceeded,
    }
}

/// Puts the connection's [`Lifecycle`] and [`Peer`] in each request's extensions, then hands it to
/// the service.
#[derive(Debug, Clone)]
struct Inject<H> {
    inner: H,
    lifecycle: Lifecycle,
    peer: SocketAddr,
}

impl<H> hyper::service::Service<Request<Incoming>> for Inject<H>
where
    H: hyper::service::Service<Request<Incoming>>,
{
    type Response = H::Response;
    type Error = H::Error;
    type Future = H::Future;

    fn call(&self, mut request: Request<Incoming>) -> Self::Future {
        request.extensions_mut().insert(self.lifecycle.clone());
        request.extensions_mut().insert(Peer(self.peer));
        self.inner.call(request)
    }
}

/// Serves one accepted connection to its end: to completion, gracefully shut down on `stop`,
/// dropped on `cut`. A connection error just ends the connection.
async fn drive<S, B>(
    stream: TcpStream,
    peer: SocketAddr,
    service: S,
    config: ServerConfig,
    lifecycle: &Lifecycle,
) where
    S: tower_service::Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    // As Go's net package does on every connection it accepts: without it, Nagle holds a small
    // write until the peer's delayed ACK arrives. A socket that refuses the option is served
    // anyway.
    let _ = stream.set_nodelay(true);
    let io = TokioIo::new(StallIo::new(stream, Some(config.write_stall_timeout)));
    let service = Inject {
        inner: TowerToHyperService::new(service),
        lifecycle: lifecycle.clone(),
        peer,
    };
    let mut builder = http1::Builder::new();
    builder.timer(TokioTimer::new());
    builder.header_read_timeout(config.header_read_timeout);
    if let Some(max) = config.max_headers {
        builder.max_headers(max);
    }
    if let Some(max) = config.max_header_size {
        builder.max_header_size(max);
    }
    if let Some(max) = config.max_buf_size {
        builder.max_buf_size(max);
    }
    if config.upgrades {
        let connection = builder.serve_connection(io, service).with_upgrades();
        run(
            connection,
            http1::UpgradeableConnection::graceful_shutdown,
            lifecycle,
        )
        .await;
    } else {
        let connection = builder.serve_connection(io, service);
        run(connection, http1::Connection::graceful_shutdown, lifecycle).await;
    }
}

/// Polls `connection` to its end, with `graceful` called on it once on `stop`; `cut` drops it.
async fn run<C>(connection: C, graceful: impl FnOnce(Pin<&mut C>), lifecycle: &Lifecycle)
where
    C: Future<Output = hyper::Result<()>>,
{
    let mut connection = std::pin::pin!(connection);
    tokio::select! {
        _ = connection.as_mut() => {}
        () = lifecycle.cut.cancelled() => {}
        () = lifecycle.stop.cancelled() => {
            graceful(connection.as_mut());
            tokio::select! {
                _ = connection.as_mut() => {}
                () = lifecycle.cut.cancelled() => {}
            }
        }
    }
}
