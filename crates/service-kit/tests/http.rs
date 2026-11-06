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

//! The HTTP server over real sockets on 127.0.0.1: an axum router served by `serve`, hand-written
//! HTTP/1.1 clients. The timeouts are real and short (a few hundred milliseconds), because tokio's
//! paused clock would auto-advance while a real socket is still in flight; each test asserts a
//! lower bound ("not cut before") and a generous upper bound ("cut by").
//!
//! Written for this crate; the scenarios are those of bitgauge's server tests
//! (bitgauge/src/server/tests.rs at d841694700a5), without its request-phase timeouts.

#![cfg(test)]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body as AxumBody, Bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use hyper::body::{Body, Frame, SizeHint};
use hyper_util::rt::TokioIo;
use service_kit::http::{Lifecycle, Outcome, Peer, ServerConfig, serve};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// The upper bound of anything a test waits for.
const PATIENCE: Duration = Duration::from_secs(10);

static CHUNK: [u8; 16_384] = [b'x'; 16_384];

/// A response body of 16 KiB chunks, `remaining` of them or for ever, that tells `dropped` when
/// the server is done with it: the connection closed or the response finished.
struct Chunks {
    remaining: Option<usize>,
    dropped: Arc<Notify>,
}

impl Body for Chunks {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        match &mut self.remaining {
            Some(0) => return Poll::Ready(None),
            Some(count) => *count -= 1,
            None => {}
        }
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(&CHUNK)))))
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

impl Drop for Chunks {
    fn drop(&mut self) {
        self.dropped.notify_one();
    }
}

struct Server {
    address: SocketAddr,
    lifecycle: Lifecycle,
    task: JoinHandle<Outcome>,
    /// Notified when a `/stream` or `/big` body is dropped.
    dropped: Arc<Notify>,
}

/// Serves a router of the routes the tests use.
async fn start(config: ServerConfig) -> Server {
    let dropped = Arc::new(Notify::new());
    let stream = Arc::clone(&dropped);
    let big = Arc::clone(&dropped);
    let router = Router::new()
        .route("/slow", get(slow))
        .route("/stream", get(move || async move { chunks(None, stream) }))
        .route(
            "/big",
            get(move || async move { chunks(Some(16_384), big) }),
        )
        .route("/peer", get(peer))
        .route("/upgrade", get(upgrade));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let lifecycle = Lifecycle::new();
    let served = lifecycle.clone();
    let task = tokio::spawn(async move { serve(listener, router, config, &served).await });
    Server {
        address,
        lifecycle,
        task,
        dropped,
    }
}

fn chunks(remaining: Option<usize>, dropped: Arc<Notify>) -> Response {
    Response::new(AxumBody::new(Chunks { remaining, dropped }))
}

async fn slow() -> &'static str {
    sleep(Duration::from_millis(400)).await;
    "done"
}

async fn peer(request: Request) -> String {
    let peer = request.extensions().get::<Peer>().expect("a Peer");
    assert!(request.extensions().get::<Lifecycle>().is_some());
    peer.0.ip().to_string()
}

/// Takes the connection over and echoes on it, as a task of the lifecycle that ends on the cut.
async fn upgrade(mut request: Request) -> Response {
    let lifecycle = request
        .extensions()
        .get::<Lifecycle>()
        .expect("a Lifecycle")
        .clone();
    let on_upgrade = hyper::upgrade::on(&mut request);
    let task = lifecycle.clone();
    lifecycle.spawn(async move {
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let mut io = TokioIo::new(upgraded);
        let mut buffer = [0_u8; 256];
        loop {
            tokio::select! {
                () = task.cut_token().cancelled() => return,
                read = io.read(&mut buffer) => match read {
                    Ok(count) if count > 0 => {
                        if io.write_all(&buffer[..count]).await.is_err() {
                            return;
                        }
                    }
                    _ => return,
                },
            }
        }
    });
    let mut response = Response::new(AxumBody::empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    response
        .headers_mut()
        .insert(header::UPGRADE, HeaderValue::from_static("echo"));
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    response
}

fn config(header: u64, stall: u64, drain: u64) -> ServerConfig {
    ServerConfig::new(
        Duration::from_millis(header),
        Duration::from_millis(stall),
        Duration::from_millis(drain),
    )
}

async fn connect(server: &Server) -> TcpStream {
    TcpStream::connect(server.address).await.expect("connect")
}

fn get_request(path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n")
}

/// Reads until the end of the response head, returning it.
async fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let count = timeout(PATIENCE, stream.read(&mut byte))
            .await
            .expect("a response head in time")
            .expect("read");
        assert_eq!(count, 1, "closed inside the head: {head:?}");
        head.push(byte[0]);
    }
    String::from_utf8(head).expect("utf-8 head")
}

/// Reads to the end of the stream, which a closed connection ends, with an error or without.
async fn read_to_close(stream: &mut TcpStream) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buffer = vec![0_u8; 65_536];
    timeout(PATIENCE, async {
        loop {
            match stream.read(&mut buffer).await {
                Ok(count) if count > 0 => all.extend_from_slice(&buffer[..count]),
                _ => return,
            }
        }
    })
    .await
    .expect("the connection to close");
    all
}

/// Whether `notify` was notified within `wait`.
async fn notified_within(notify: &Notify, wait: Duration) -> bool {
    timeout(wait, notify.notified()).await.is_ok()
}

async fn finish(server: Server) -> Outcome {
    server.lifecycle.stop();
    timeout(PATIENCE, server.task)
        .await
        .expect("serve to return")
        .expect("serve not to panic")
}

/// The request carries the peer and the lifecycle.
#[tokio::test]
async fn requests_carry_the_peer_and_the_lifecycle() {
    let server = start(config(5_000, 5_000, 2_000)).await;
    let mut client = connect(&server).await;
    client
        .write_all(get_request("/peer").as_bytes())
        .await
        .expect("write");
    let head = read_head(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let mut body = [0_u8; 9];
    client.read_exact(&mut body).await.expect("body");
    assert_eq!(&body, b"127.0.0.1");
    drop(client);
    assert!(matches!(finish(server).await, Outcome::Drained));
}

/// A client that sends half a head is cut at the header-read timeout, not before.
#[tokio::test]
async fn a_slow_header_client_is_cut_at_the_header_timeout() {
    let server = start(config(300, 5_000, 2_000)).await;
    let mut client = connect(&server).await;
    client
        .write_all(b"GET /peer HTTP/1.1\r\nHost: te")
        .await
        .expect("write");
    let started = Instant::now();
    let _ = read_to_close(&mut client).await;
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert!(matches!(finish(server).await, Outcome::Drained));
}

/// A request head over `max_header_size` is refused with 431.
#[tokio::test]
async fn an_oversized_head_is_refused() {
    let server = start(config(5_000, 5_000, 2_000).max_header_size(1024)).await;
    let mut client = connect(&server).await;
    let request = format!(
        "GET /peer HTTP/1.1\r\nHost: test\r\nX-Big: {}\r\n\r\n",
        "a".repeat(4096)
    );
    client.write_all(request.as_bytes()).await.expect("write");
    let response = String::from_utf8_lossy(&read_to_close(&mut client).await).into_owned();
    assert!(response.starts_with("HTTP/1.1 431"), "{response}");
    assert!(matches!(finish(server).await, Outcome::Drained));
}

/// A client that stops reading a streamed response is cut at the write-stall timeout. The handler
/// had returned long before: the stall is in the body, after it.
#[tokio::test]
async fn a_client_that_stops_reading_a_stream_is_cut_at_the_write_stall_timeout() {
    let server = start(config(5_000, 400, 2_000)).await;
    let mut client = connect(&server).await;
    client
        .write_all(get_request("/stream").as_bytes())
        .await
        .expect("write");
    let head = read_head(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let started = Instant::now();
    // Not reading: the socket buffers fill, the write stays pending, and the deadline runs.
    assert!(
        notified_within(&server.dropped, PATIENCE).await,
        "the body was never dropped"
    );
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
    drop(client);
    assert!(matches!(finish(server).await, Outcome::Drained));
}

/// The same for a finite, large body.
#[tokio::test]
async fn a_client_that_stops_reading_a_large_body_is_cut_at_the_write_stall_timeout() {
    let server = start(config(5_000, 400, 2_000)).await;
    let mut client = connect(&server).await;
    client
        .write_all(get_request("/big").as_bytes())
        .await
        .expect("write");
    let _ = read_head(&mut client).await;
    assert!(
        notified_within(&server.dropped, PATIENCE).await,
        "the body was never dropped"
    );
    // The connection is closed: reading what was buffered ends.
    let _ = read_to_close(&mut client).await;
    assert!(matches!(finish(server).await, Outcome::Drained));
}

/// A client that reads slowly but steadily is never cut, whatever the response's total time.
#[tokio::test]
async fn a_slow_but_reading_client_is_not_cut() {
    let server = start(config(5_000, 500, 2_000)).await;
    let mut client = connect(&server).await;
    client
        .write_all(get_request("/stream").as_bytes())
        .await
        .expect("write");
    let _ = read_head(&mut client).await;
    let started = Instant::now();
    let mut buffer = vec![0_u8; 65_536];
    while started.elapsed() < Duration::from_millis(1_800) {
        let count = client.read(&mut buffer).await.expect("read");
        assert!(count > 0, "cut after {:?}", started.elapsed());
        sleep(Duration::from_millis(5)).await;
    }
    assert!(!notified_within(&server.dropped, Duration::from_millis(50)).await);
    drop(client);
    assert!(matches!(finish(server).await, Outcome::Drained));
}

/// A request in flight at `stop` is answered in full, with `Connection: close`; an idle
/// connection is closed at once; `serve` returns `Drained` within the budget.
#[tokio::test]
async fn stop_drains_a_request_in_flight() {
    let server = start(config(5_000, 5_000, 5_000)).await;
    let mut busy = connect(&server).await;
    let mut idle = connect(&server).await;
    busy.write_all(get_request("/slow").as_bytes())
        .await
        .expect("write");
    sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    server.lifecycle.stop();
    assert!(read_to_close(&mut idle).await.is_empty());
    let response = String::from_utf8(read_to_close(&mut busy).await).expect("utf-8");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.to_ascii_lowercase().contains("connection: close"));
    assert!(response.ends_with("done"), "{response}");
    let outcome = timeout(PATIENCE, server.task)
        .await
        .expect("serve to return")
        .expect("no panic");
    assert!(matches!(outcome, Outcome::Drained), "{outcome:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(!server.lifecycle.cut_token().is_cancelled());
}

/// After `stop` the listener is gone: a new connection is refused.
#[tokio::test]
async fn stop_closes_the_listener() {
    let server = start(config(5_000, 5_000, 2_000)).await;
    let address = server.address;
    assert!(matches!(finish(server).await, Outcome::Drained));
    assert!(TcpStream::connect(address).await.is_err());
}

/// A transfer still running at the drain deadline is cut: the outcome says so, and the connection
/// is closed.
#[tokio::test]
async fn the_cut_closes_a_transfer_that_outlives_the_drain() {
    let server = start(config(5_000, 60_000, 400)).await;
    let mut client = connect(&server).await;
    client
        .write_all(get_request("/stream").as_bytes())
        .await
        .expect("write");
    let _ = read_head(&mut client).await;
    let started = Instant::now();
    server.lifecycle.stop();
    let outcome = timeout(PATIENCE, server.task)
        .await
        .expect("serve to return")
        .expect("no panic");
    assert!(matches!(outcome, Outcome::DeadlineExceeded), "{outcome:?}");
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(350), "{elapsed:?}");
    assert!(server.lifecycle.cut_token().is_cancelled());
    assert!(notified_within(&server.dropped, PATIENCE).await);
    let _ = read_to_close(&mut client).await;
}

async fn upgrade_client(server: &Server) -> TcpStream {
    let mut client = connect(server).await;
    client
        .write_all(
            b"GET /upgrade HTTP/1.1\r\nHost: test\r\nConnection: upgrade\r\nUpgrade: echo\r\n\r\n",
        )
        .await
        .expect("write");
    let head = read_head(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    client
}

async fn echo(client: &mut TcpStream, text: &[u8]) {
    client.write_all(text).await.expect("write");
    let mut back = vec![0_u8; text.len()];
    timeout(PATIENCE, client.read_exact(&mut back))
        .await
        .expect("an echo in time")
        .expect("read");
    assert_eq!(back, text);
}

/// An upgraded connection passes bytes both ways.
#[tokio::test]
async fn an_upgrade_passes_through() {
    let server = start(config(5_000, 5_000, 1_000).with_upgrades()).await;
    let mut client = upgrade_client(&server).await;
    echo(&mut client, b"hello").await;
    echo(&mut client, b"again").await;
    drop(client);
    assert!(matches!(finish(server).await, Outcome::Drained));
}

/// An upgraded connection spawned on the lifecycle survives the graceful window: `stop` does not
/// touch it. At the deadline the cut closes it, and `serve` says it was cut.
#[tokio::test]
async fn an_upgraded_connection_survives_stop_and_is_cut_at_the_deadline() {
    let server = start(config(5_000, 5_000, 800).with_upgrades()).await;
    let mut client = upgrade_client(&server).await;
    echo(&mut client, b"before").await;
    let started = Instant::now();
    server.lifecycle.stop();
    sleep(Duration::from_millis(300)).await;
    assert!(
        !server.task.is_finished(),
        "serve returned inside the window"
    );
    assert!(!server.lifecycle.cut_token().is_cancelled());
    echo(&mut client, b"during").await;
    let outcome = timeout(PATIENCE, server.task)
        .await
        .expect("serve to return")
        .expect("no panic");
    assert!(matches!(outcome, Outcome::DeadlineExceeded), "{outcome:?}");
    assert!(started.elapsed() >= Duration::from_millis(750));
    assert!(read_to_close(&mut client).await.is_empty());
}

/// A server that has no upgrade support turns the upgrade down: the handler cannot take the
/// connection, so the echo task ends at once and nothing echoes.
#[tokio::test]
async fn upgrades_are_off_unless_enabled() {
    let server = start(config(5_000, 5_000, 1_000)).await;
    let mut client = connect(&server).await;
    client
        .write_all(
            b"GET /upgrade HTTP/1.1\r\nHost: test\r\nConnection: upgrade\r\nUpgrade: echo\r\n\r\n",
        )
        .await
        .expect("write");
    let _ = read_head(&mut client).await;
    client.write_all(b"hello").await.expect("write");
    let mut back = [0_u8; 5];
    let read = timeout(Duration::from_millis(500), client.read(&mut back)).await;
    assert!(!matches!(read, Ok(Ok(5))), "echoed without upgrades");
    drop(client);
    assert!(matches!(finish(server).await, Outcome::Drained));
}
