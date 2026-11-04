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

//! The accept loop and the drain on real loopback sockets: Go's shutdown probe, the drain's
//! deadline, a fatal accept error, and the stop at once that Go's `serveUntilDone` makes of one.
//!
//! Extracted from swarm-device-access/src/server/tests/shutdown.rs at 8b4ab3a3e417 and
//! swarm-scheduler-exporter/src/server/tests/shutdown.rs at c9e79afa8810: the drain tests both
//! share; each program's own wrapper (`serve_until_done`, `MetricsServer`) and its tests stay
//! there.

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::net::TcpStream;
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use socket2::Socket;

use super::*;
use crate::bind;

/// How long a test waits for something that should happen at once.
const BOUND: Duration = Duration::from_secs(5);

async fn get(stream: &mut TcpStream, path: &str) -> String {
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = vec![0_u8; 4096];
    let read = stream.read(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf[..read]).into_owned()
}

/// Go's shutdown probe: after the signal new connects are refused, an idle keep-alive
/// connection is closed at once, and the request in flight is answered, with `Connection:
/// close`, before the drain ends.
#[tokio::test]
async fn the_drain_waits_for_the_request_in_flight_and_refuses_new_ones() {
    let listener = bind("127.0.0.1:0", "").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        Arc::new(slow_server(Duration::from_millis(600))),
        EXPORTER,
        async {
            let _ = stopped.await;
        },
        || Instant::now() + Duration::from_secs(10),
        |error| panic!("{error}"),
    ));
    let mut idle = TcpStream::connect(address).await.unwrap();
    assert!(
        get(&mut idle, "/healthz")
            .await
            .starts_with("HTTP/1.1 200 OK")
    );
    let mut in_flight = TcpStream::connect(address).await.unwrap();
    in_flight
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let signalled = Instant::now();
    stop.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let refused = TcpStream::connect(address).await.unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
    let mut rest = Vec::new();
    assert_eq!(
        idle.read_to_end(&mut rest).await.unwrap(),
        0,
        "the idle keep-alive closed at once"
    );
    assert!(
        !served.is_finished(),
        "the drain must wait for the request in flight"
    );
    let mut response = Vec::new();
    in_flight.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("\r\nConnection: close\r\n"), "{response}");
    let served = served.await.unwrap();
    assert_eq!(served.drained, Drained::Clean);
    assert!(signalled.elapsed() < Duration::from_millis(900));
}

/// The drain gives up at the deadline it is handed, not after a grace period of its own.
#[tokio::test]
async fn the_drain_stops_at_the_deadline() {
    let listener = bind("127.0.0.1:0", "").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        Arc::new(slow_server(Duration::from_secs(30))),
        EXPORTER,
        async {
            let _ = stopped.await;
        },
        || Instant::now() + Duration::from_millis(300),
        |error| panic!("{error}"),
    ));
    let mut in_flight = TcpStream::connect(address).await.unwrap();
    in_flight
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let signalled = Instant::now();
    stop.send(()).unwrap();
    let served = served.await.unwrap();
    assert_eq!(served.drained, Drained::DeadlineExceeded);
    let took = signalled.elapsed();
    assert!(
        // Well short of any grace period of its own, with room for a loaded runner.
        took >= Duration::from_millis(300) && took < Duration::from_secs(3),
        "{took:?}"
    );
    assert!(served.deadline <= Instant::now());
}

/// Go: swarm-scheduler-exporter's `TestServeHTTP_ShutdownWaitsForInFlightRequest`: once shutdown
/// starts, the server keeps waiting for the request in flight, and returns once it is answered.
#[tokio::test]
async fn the_shutdown_waits_for_the_request_in_flight() {
    let listener = bind("127.0.0.1:0", "").await.unwrap();
    let address = listener.local_addr().unwrap();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (route_started, route_release) = (Arc::clone(&started), Arc::clone(&release));
    let slow: Arc<dyn Fn() -> ReplyFuture + Send + Sync> = Arc::new(move || {
        let (started, release) = (Arc::clone(&route_started), Arc::clone(&route_release));
        Box::pin(async move {
            started.notify_one();
            release.notified().await;
            slow_done()
        })
    });
    let waiting = server_with(text_metrics(b""), slow, capture_time());
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let served = tokio::spawn(async move {
        serve(
            listener,
            Arc::new(waiting),
            EXPORTER,
            shutdown.cancelled_owned(),
            || Instant::now() + Duration::from_secs(10),
            |error| panic!("{error}"),
        )
        .await
    });
    let response = tokio::spawn(async move {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        String::from_utf8_lossy(&response).into_owned()
    });
    tokio::time::timeout(BOUND, started.notified())
        .await
        .expect("request not received");
    cancel.cancel();
    // It must keep waiting for the request in flight.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !served.is_finished(),
        "returned while a request was in flight"
    );
    release.notify_one();
    let served = tokio::time::timeout(BOUND, served).await.unwrap().unwrap();
    assert_eq!(served.drained, Drained::Clean);
    let response = tokio::time::timeout(BOUND, response)
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
}

/// A handle on `listener`'s socket (a duplicate descriptor) that makes its `accept` fail with
/// `EINVAL`, an error Go's `Serve` does not retry: a listening socket shut down for reading wakes
/// its `accept` with it on Linux.
fn accept_breaker(listener: &TcpListener) -> Socket {
    socket2::SockRef::from(listener).try_clone().unwrap()
}

fn break_accept(breaker: &Socket) {
    breaker.shutdown(std::net::Shutdown::Read).unwrap();
}

/// A fatal accept error is handed to `on_error` at once, in Go's words, and closes the listener;
/// the open connections are still served until the shutdown, as Go's are until `Shutdown`.
#[tokio::test]
async fn a_fatal_accept_error_closes_the_listener_and_serves_on_until_the_shutdown() {
    let listener = bind("127.0.0.1:0", "").await.unwrap();
    let address = listener.local_addr().unwrap();
    let breaker = accept_breaker(&listener);
    let (failed, error) = oneshot::channel::<io::Error>();
    let (stop, stopped) = oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        Arc::new(healthy_server()),
        EXPORTER,
        async {
            let _ = stopped.await;
        },
        || Instant::now() + Duration::from_secs(10),
        move |error| failed.send(error).unwrap(),
    ));
    let mut open = TcpStream::connect(address).await.unwrap();
    assert!(
        get(&mut open, "/healthz")
            .await
            .starts_with("HTTP/1.1 200 OK")
    );
    break_accept(&breaker);
    let error = tokio::time::timeout(BOUND, error).await.unwrap().unwrap();
    assert_eq!(
        error.to_string(),
        format!("accept tcp {address}: accept4: invalid argument")
    );
    assert_eq!(
        TcpStream::connect(address).await.unwrap_err().kind(),
        io::ErrorKind::ConnectionRefused
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!served.is_finished(), "it must serve on until the shutdown");
    assert!(
        get(&mut open, "/healthz")
            .await
            .starts_with("HTTP/1.1 200 OK"),
        "the open connection is still served"
    );
    drop(open);
    stop.send(()).unwrap();
    let served = tokio::time::timeout(BOUND, served).await.unwrap().unwrap();
    assert_eq!(served.drained, Drained::Clean);
}

/// Go's `serveUntilDone` drains as soon as `Serve` fails: a shutdown future that also resolves
/// once `on_error` ran does that, with the drain's deadline computed then, and the error kept
/// for after the drain.
#[tokio::test]
async fn an_on_error_that_stops_the_server_drains_at_once() {
    let listener = bind("127.0.0.1:0", "").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let failed = CancellationToken::new();
    let error = Arc::new(Mutex::new(None));
    let deadlines = Arc::new(AtomicUsize::new(0));
    break_accept(&accept_breaker(&listener));
    let shutdown = {
        let (stop, failed) = (stop.clone(), failed.clone());
        async move {
            tokio::select! {
                () = stop.cancelled() => {}
                () = failed.cancelled() => {}
            }
        }
    };
    let on_error = {
        let error = Arc::clone(&error);
        move |cause: io::Error| {
            *error.lock().unwrap() = Some(cause);
            failed.cancel();
        }
    };
    let counted = Arc::clone(&deadlines);
    let served = tokio::time::timeout(
        BOUND,
        serve(
            listener,
            Arc::new(healthy_server()),
            EXPORTER,
            shutdown,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Instant::now() + Duration::from_secs(10)
            },
            on_error,
        ),
    )
    .await
    .expect("serve did not return after the accept error");
    assert_eq!(served.drained, Drained::Clean);
    assert_eq!(deadlines.load(Ordering::SeqCst), 1);
    assert!(!stop.is_cancelled());
    assert_eq!(
        error.lock().unwrap().take().unwrap().to_string(),
        format!("accept tcp {address}: accept4: invalid argument")
    );
}

/// The accept errors Go's `Serve` retries: `syscall.Errno.Temporary`'s and `ECONNABORTED`.
#[test]
fn a_retried_accept_error_is_temporary() {
    for errno in [4, 11, 23, 24, 103, 110] {
        assert!(temporary(&io::Error::from_raw_os_error(errno)), "{errno}");
    }
    for errno in [9, 22, 98] {
        assert!(!temporary(&io::Error::from_raw_os_error(errno)), "{errno}");
    }
}
