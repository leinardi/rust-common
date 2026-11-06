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

//! The service's answers over an in-memory connection, and the helpers of the timeout, shutdown
//! and wire tests: a server with a health route, a metrics route, a sniffed route and a slow one.
//!
//! Extracted from swarm-scheduler-exporter/src/server/tests.rs at c9e79afa8810 and
//! swarm-device-access/src/server/tests.rs at 8b4ab3a3e417: the framing and scrape tests, which
//! the two copies share, with each program's own routes replaced by these.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use clock::FakeClock;
use hyper::StatusCode;
use jiff::Timestamp;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};

use super::*;

mod shutdown;
mod timeouts;
mod wire;

/// swarm-scheduler-exporter's server timeouts.
const EXPORTER: Timeouts = Timeouts {
    read_header: Duration::from_secs(5),
    read: Duration::from_secs(10),
    write: Duration::from_secs(15),
    idle: Duration::from_mins(1),
};

/// swarm-device-access's: the write timeout is 30 s.
const DAEMON: Timeouts = Timeouts {
    write: Duration::from_secs(30),
    ..EXPORTER
};

/// The `Content-Type` promhttp answers a scrape without `Accept` with.
const TEXT_EXPOSITION: &str = "text/plain; version=0.0.4; charset=utf-8; escaping=underscores";

/// The time of the Go captures, so `Date` compares exactly.
fn capture_time() -> Timestamp {
    "2026-10-25T08:44:07Z".parse().unwrap()
}

/// The `/metrics` answer of a test server for a scrape.
type MetricsFn = Arc<dyn Fn(&MetricsRequest) -> Reply + Send + Sync>;

/// A test server: `/healthz` answers `ok` with its type set, as swarm-scheduler-exporter's does;
/// `/readyz` answers `ok` with its type sniffed, as swarm-device-access's does; `/metrics` answers
/// with `metrics`; `/slow` waits for `slow`, then answers `slow done`.
fn server_with(
    metrics: MetricsFn,
    slow: Arc<dyn Fn() -> ReplyFuture + Send + Sync>,
    clock: Timestamp,
) -> Server {
    let mux = Mux::new()
        .route("/metrics", move |request| {
            metrics(&MetricsRequest::of(request))
        })
        .route("/healthz", |_| {
            Reply::new(
                StatusCode::OK,
                vec![(CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN))],
                "ok\n",
            )
        })
        .route("/readyz", |_| Reply::sniffed(StatusCode::OK, "ok"))
        .route_async("/slow", move |_| slow());
    Server::new(mux, Arc::new(FakeClock::new(clock)), |_| {})
}

/// A route that waits `duration`, then answers `slow done`.
fn sleeping(duration: Duration) -> Arc<dyn Fn() -> ReplyFuture + Send + Sync> {
    Arc::new(move || {
        Box::pin(async move {
            tokio::time::sleep(duration).await;
            slow_done()
        })
    })
}

fn slow_done() -> Reply {
    Reply::new(
        StatusCode::OK,
        vec![(CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN))],
        "slow done\n",
    )
}

/// A `/metrics` answer of `body` in the text format.
fn text_metrics(body: &'static [u8]) -> MetricsFn {
    Arc::new(move |_| Reply::metrics(TEXT_EXPOSITION, None, body))
}

fn healthy_server() -> Server {
    slow_server(Duration::ZERO)
}

fn slow_server(duration: Duration) -> Server {
    server_with(text_metrics(b"m 1\n"), sleeping(duration), capture_time())
}

/// Serves one connection of `server` over a duplex and returns the client end.
fn connect(server: Server, timeouts: Timeouts) -> DuplexStream {
    let (client, server_io) = tokio::io::duplex(1 << 20);
    let serving = connection(server_io, Arc::new(server), timeouts);
    tokio::spawn(async move {
        let _ = serving.await;
    });
    client
}

/// Sends `request` on a fresh connection of `server` and returns everything the server wrote
/// before it closed.
async fn exchange(server: Server, request: &[u8]) -> Vec<u8> {
    let mut client = connect(server, EXPORTER);
    client.write_all(request).await.unwrap();
    let mut response = Vec::new();
    let _ = client.read_to_end(&mut response).await;
    response
}

async fn exchange_text(server: Server, request: &str) -> String {
    String::from_utf8(exchange(server, request.as_bytes()).await).unwrap()
}

/// Go: swarm-scheduler-exporter's `TestHealthHandler_Healthy`: a handler that set its type.
#[tokio::test(start_paused = true)]
async fn a_typed_answer_is_go_s() {
    let response = exchange_text(
        healthy_server(),
        "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nDate: Sun, 25 Oct 2026 \
         08:44:07 GMT\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n"
    );
}

/// A handler that set no type gets the sniffed one after `Content-Length`, as Go writes it
/// (swarm-device-access's `/healthz` and `/readyz` captures).
#[tokio::test(start_paused = true)]
async fn a_sniffed_type_follows_the_length() {
    let response = exchange_text(
        healthy_server(),
        "GET /readyz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nDate: Sun, 25 Oct 2026 08:44:07 GMT\r\nContent-Length: 2\r\n\
         Content-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\nok"
    );
}

/// `OPTIONS *` is Go's global options handler, `CONNECT` and an unknown path Go's 404, an unclean
/// path a 307 to the cleaned one, and a bad escape a 400 that closes the connection.
#[tokio::test(start_paused = true)]
async fn the_mux_answers_as_go_s() {
    let options = exchange_text(
        healthy_server(),
        "OPTIONS * HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(
        options,
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nDate: Sun, 25 Oct 2026 08:44:07 GMT\r\n\
         Connection: close\r\n\r\n"
    );
    let connect = exchange_text(
        healthy_server(),
        "CONNECT x:1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(
        connect.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "{connect}"
    );
    let unknown = exchange_text(
        Server::new(Mux::new(), Arc::new(FakeClock::new(capture_time())), |_| {}),
        "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(
        unknown.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "{unknown}"
    );
    let redirect = exchange_text(
        healthy_server(),
        "POST /a/../healthz?x=1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(
        redirect,
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: /healthz?x=1\r\nDate: Sun, 25 Oct 2026 \
         08:44:07 GMT\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let bad = exchange_text(healthy_server(), "GET /%zz HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert_eq!(
        bad,
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nDate: Sun, 25 Oct \
         2026 08:44:07 GMT\r\nContent-Length: 15\r\nConnection: close\r\n\r\n400 Bad Request"
    );
}

/// Go's framing: up to 2 KiB a `Content-Length`, above it chunks of 2 KiB.
#[tokio::test(start_paused = true)]
async fn bodies_above_2_kib_are_chunked() {
    let server = |body: &'static [u8]| {
        server_with(text_metrics(body), sleeping(Duration::ZERO), capture_time())
    };
    let exact = vec![b'x'; 2048].leak();
    let response = exchange(
        server(exact),
        b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("\r\nContent-Length: 2048\r\n"), "{text}");
    assert!(!text.contains("Transfer-Encoding"), "{text}");

    let over = vec![b'x'; 4097].leak();
    let response = exchange(
        server(over),
        b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(
        head.ends_with("\r\nConnection: close\r\nTransfer-Encoding: chunked"),
        "{head}"
    );
    assert!(!head.contains("Content-Length"), "{head}");
    let x2048 = "x".repeat(2048);
    assert_eq!(
        body,
        format!("800\r\n{x2048}\r\n800\r\n{x2048}\r\n1\r\nx\r\n0\r\n\r\n")
    );

    // HEAD: neither header, as Go writes it.
    let response = exchange(
        server(over),
        b"HEAD /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(!text.contains("Content-Length"), "{text}");
    assert!(!text.contains("Transfer-Encoding"), "{text}");
    assert!(text.ends_with("\r\n\r\n"), "{text}");
}

/// promhttp reads the first `Accept`, every `Accept-Encoding` and the `name[]` filter; a gzip
/// answer carries its `Content-Encoding` ahead of the `Content-Type`.
#[tokio::test(start_paused = true)]
async fn the_scrape_request_reaches_the_metrics_function() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let metrics: MetricsFn = Arc::new(move |scrape: &MetricsRequest| {
        recorded.lock().unwrap().push(scrape.clone());
        Reply::metrics(TEXT_EXPOSITION, Some("gzip"), &b"z"[..])
    });
    let response = exchange_text(
        server_with(metrics, sleeping(Duration::ZERO), capture_time()),
        "GET /metrics?name[]=up&name%5B%5D=x&other=1 HTTP/1.1\r\nHost: x\r\nAccept: text/plain\r\n\
         Accept: application/json\r\nAccept-Encoding: gzip\r\nAccept-Encoding: br\r\n\
         Connection: close\r\n\r\n",
    )
    .await;
    assert!(
        response.starts_with(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Type: text/plain; \
             version=0.0.4; charset=utf-8; escaping=underscores\r\nDate: "
        ),
        "{response}"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(
        *seen,
        [MetricsRequest {
            accept: Some("text/plain".to_owned()),
            accept_encoding: vec!["gzip".to_owned(), "br".to_owned()],
            names: Some(BTreeSet::from(["up".to_owned(), "x".to_owned()])),
        }]
    );
}

/// Without `name[]`, no filter.
#[tokio::test(start_paused = true)]
async fn a_scrape_without_names_has_no_filter() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let metrics: MetricsFn = Arc::new(move |scrape: &MetricsRequest| {
        recorded.lock().unwrap().push(scrape.clone());
        Reply::metrics(TEXT_EXPOSITION, None, Bytes::new())
    });
    exchange(
        server_with(metrics, sleeping(Duration::ZERO), capture_time()),
        b"GET /metrics?a=b HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(*seen.lock().unwrap(), [MetricsRequest::default()]);
}

/// Go's `log.Printf` prefix, in the zone given.
#[test]
fn log_lines_are_go_s() {
    let now: Timestamp = "2026-10-25T08:44:07.9Z".parse().unwrap();
    let zone = jiff::tz::TimeZone::fixed(jiff::tz::offset(2));
    assert_eq!(
        log_printf_line(now, &zone, "http: Accept error"),
        "2026/10/25 10:44:07 http: Accept error\n"
    );
}
