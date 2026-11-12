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

//! The log streams against the fake engine.

use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use crate::demux::{Framing, Pipe};
use crate::logs::{self, ContainerLogsOptions, LogItem, ServiceLogsOptions, StreamEnd};
use crate::test_util::{FakeEngine, Reply};
use crate::{CancellationToken, Config, ErrorKind};

const SECS: Duration = Duration::from_secs(5);

fn config_of(engine: &FakeEngine) -> Config {
    Config::unix(engine.socket().to_str().unwrap())
}

fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![stream, 0, 0, 0];
    bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// A `200` whose body is `body`, read until the connection closes.
fn until_close(body: &[u8]) -> Vec<u8> {
    let mut bytes = b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.multiplexed-stream\r\nConnection: close\r\n\r\n".to_vec();
    bytes.extend_from_slice(body);
    bytes
}

/// A chunked `200` whose chunks are `chunks`, with the terminating chunk when `complete`.
fn chunked(chunks: &[&[u8]], complete: bool) -> Vec<u8> {
    let mut bytes = b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.multiplexed-stream\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for chunk in chunks {
        bytes.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        bytes.extend_from_slice(chunk);
        bytes.extend_from_slice(b"\r\n");
    }
    if complete {
        bytes.extend_from_slice(b"0\r\n\r\n");
    }
    bytes
}

fn every_option() -> ContainerLogsOptions {
    ContainerLogsOptions {
        stdout: true,
        stderr: true,
        follow: true,
        timestamps: true,
        tail: Some("7".to_owned()),
        since: Some("1767225600.000000000".to_owned()),
        until: Some("1767312000.000000000".to_owned()),
    }
}

/// Everything until the end, which must be the last item and come once.
async fn collect(mut receiver: mpsc::Receiver<LogItem>) -> (Vec<(Pipe, Vec<u8>)>, StreamEnd) {
    let mut frames = Vec::new();
    loop {
        let item = tokio::time::timeout(SECS, receiver.recv())
            .await
            .expect("an item in time")
            .expect("an end before the channel closes");
        match item {
            LogItem::Frame(frame) => frames.push((frame.pipe, frame.bytes.to_vec())),
            LogItem::End(end) => {
                assert!(receiver.recv().await.is_none(), "an item after the end");
                return (frames, end);
            }
        }
    }
}

/// The queries are moby's Go client's, as runhold's wire pins hold them: flags only when set,
/// `tail` not for `all` or empty, the keys sorted; a service never sends `until`.
#[tokio::test]
async fn the_log_queries_are_moby_s() {
    let engine = FakeEngine::start(&[
        ("/containers/c1/logs", Reply::Raw(until_close(b""))),
        ("/services/s1/logs", Reply::Raw(until_close(b""))),
    ])
    .unwrap();
    let config = config_of(&engine).api_version("1.44").unwrap();
    let cancel = CancellationToken::new();
    let all = ContainerLogsOptions {
        stdout: true,
        stderr: true,
        tail: Some("all".to_owned()),
        ..ContainerLogsOptions::default()
    };
    for options in [every_option(), all, ContainerLogsOptions::default()] {
        let receiver = logs::container_logs(
            &config,
            " c1 ",
            &options,
            Framing::Raw,
            SECS,
            4,
            cancel.clone(),
        )
        .await
        .unwrap();
        collect(receiver).await;
    }
    let service = ServiceLogsOptions {
        stdout: true,
        stderr: true,
        follow: true,
        timestamps: true,
        details: true,
        tail: Some("7".to_owned()),
        since: Some("1767225600".to_owned()),
    };
    let empty = ServiceLogsOptions {
        tail: Some(String::new()),
        since: Some(String::new()),
        ..ServiceLogsOptions::default()
    };
    for options in [service, empty] {
        let receiver = logs::service_logs(
            &config,
            "s1",
            &options,
            Framing::Multiplexed,
            SECS,
            4,
            cancel.clone(),
        )
        .await
        .unwrap();
        collect(receiver).await;
    }
    let seen: Vec<(String, String)> = engine
        .requests()
        .iter()
        .map(|request| (request.method.clone(), request.raw_path().to_owned()))
        .collect();
    let get = |path: &str| ("GET".to_owned(), path.to_owned());
    assert_eq!(
        seen,
        [
            get(
                "/v1.44/containers/c1/logs?follow=1&since=1767225600.000000000&stderr=1&stdout=1&tail=7&timestamps=1&until=1767312000.000000000"
            ),
            get("/v1.44/containers/c1/logs?stderr=1&stdout=1"),
            get("/v1.44/containers/c1/logs"),
            get(
                "/v1.44/services/s1/logs?details=1&follow=1&since=1767225600&stderr=1&stdout=1&tail=7&timestamps=1"
            ),
            get("/v1.44/services/s1/logs"),
        ]
    );
}

/// An empty ID is refused before sending, with moby's text.
#[tokio::test]
async fn an_empty_id_is_refused_before_sending() {
    let engine = FakeEngine::start(&[]).unwrap();
    let config = config_of(&engine);
    let cancel = CancellationToken::new();
    let error = logs::container_logs(
        &config,
        "",
        &every_option(),
        Framing::Raw,
        SECS,
        1,
        cancel.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid container name or ID: value is empty"),
        "{error}"
    );
    let error = logs::service_logs(
        &config,
        " ",
        &ServiceLogsOptions::default(),
        Framing::Raw,
        SECS,
        1,
        cancel,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid service name or ID: value is empty"),
        "{error}"
    );
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(engine.requests().is_empty());
}

/// A multiplexed body is demultiplexed, frames split across chunks come out whole, and a body
/// that ends cleanly ends with `Eof`, its partial last frame dropped.
#[tokio::test]
async fn a_multiplexed_body_is_split_and_ends_with_eof() {
    let body = [
        frame(1, b"\x01one\n"),
        frame(2, b"two\n"),
        frame(1, b"three"),
    ]
    .concat();
    let (first, rest) = body.split_at(5);
    let (second, third) = rest.split_at(9);
    let partial = &frame(1, b"cut")[..6];
    let engine = FakeEngine::start(&[(
        "/containers/c/logs",
        Reply::Raw(chunked(&[first, second, third, partial], true)),
    )])
    .unwrap();
    let receiver = logs::container_logs(
        &config_of(&engine),
        "c",
        &every_option(),
        Framing::Multiplexed,
        SECS,
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (frames, end) = collect(receiver).await;
    assert_eq!(
        frames,
        [
            (Pipe::Stdout, b"\x01one\n".to_vec()),
            (Pipe::Stderr, b"two\n".to_vec()),
            (Pipe::Stdout, b"three".to_vec()),
        ]
    );
    assert!(matches!(end, StreamEnd::Eof), "{end:?}");
}

/// A raw body (a TTY container) is stdout as it is: a line starting with `\x01` is not taken
/// for a header.
#[tokio::test]
async fn a_raw_body_is_never_read_as_headers() {
    let line = b"\x01\x00\x00\x00\x00\x00\x00\x02hello\n";
    let engine =
        FakeEngine::start(&[("/containers/tty/logs", Reply::Raw(until_close(line)))]).unwrap();
    let receiver = logs::container_logs(
        &config_of(&engine),
        "tty",
        &every_option(),
        Framing::Raw,
        SECS,
        4,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (frames, end) = collect(receiver).await;
    let bytes: Vec<u8> = frames
        .iter()
        .flat_map(|(pipe, bytes)| {
            assert_eq!(*pipe, Pipe::Stdout);
            bytes.clone()
        })
        .collect();
    assert_eq!(bytes, line);
    assert!(matches!(end, StreamEnd::Eof), "{end:?}");
}

/// A connection cut mid-frame (a chunked body without its last chunk) is `Failed` with
/// `Unavailable`; an error the engine sends in the stream is `Failed` with its text.
#[tokio::test]
async fn a_cut_connection_and_an_engine_error_fail_the_stream() {
    let whole = frame(1, b"kept");
    let half = &frame(1, b"lost")[..9];
    let engine = FakeEngine::start(&[
        (
            "/containers/cut/logs",
            Reply::Raw(chunked(&[&whole, half], false)),
        ),
        (
            "/containers/oops/logs",
            Reply::Raw(chunked(
                &[&frame(3, b"driver failed"), &frame(1, b"never")],
                true,
            )),
        ),
    ])
    .unwrap();
    let config = config_of(&engine);
    let receiver = logs::container_logs(
        &config,
        "cut",
        &every_option(),
        Framing::Multiplexed,
        SECS,
        4,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (frames, end) = collect(receiver).await;
    assert_eq!(frames, [(Pipe::Stdout, b"kept".to_vec())]);
    let StreamEnd::Failed(error) = end else {
        panic!("{end:?}")
    };
    assert_eq!(error.kind(), ErrorKind::Unavailable, "{error}");

    let receiver = logs::container_logs(
        &config,
        "oops",
        &every_option(),
        Framing::Multiplexed,
        SECS,
        4,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (frames, end) = collect(receiver).await;
    assert!(frames.is_empty(), "{frames:?}");
    let StreamEnd::Failed(error) = end else {
        panic!("{end:?}")
    };
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(
        error
            .to_string()
            .contains("error from daemon in stream: driver failed"),
        "{error}"
    );
}

/// A token fired mid-stream ends it with `Cancelled`, even while the receiver is full and not
/// read: the end comes once there is room, after the frames already sent.
#[tokio::test]
async fn cancelling_mid_stream_ends_with_cancelled() {
    let body = [frame(1, b"a"), frame(1, b"b"), frame(1, b"c")].concat();
    let engine =
        FakeEngine::start(&[("/services/s/logs", Reply::Held(chunked(&[&body], false)))]).unwrap();
    let cancel = CancellationToken::new();
    let mut receiver = logs::service_logs(
        &config_of(&engine),
        "s",
        &ServiceLogsOptions::default(),
        Framing::Multiplexed,
        SECS,
        1,
        cancel.clone(),
    )
    .await
    .unwrap();
    // Wait until the task has filled the channel and blocks on the second frame.
    tokio::time::timeout(SECS, async {
        while receiver.capacity() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the channel fills");
    cancel.cancel();
    let (frames, end) = collect_after(&mut receiver).await;
    assert_eq!(frames, [(Pipe::Stdout, b"a".to_vec())]);
    assert!(matches!(end, StreamEnd::Cancelled), "{end:?}");

    // Quiet, followed: the token ends the wait for the body too.
    let engine =
        FakeEngine::start(&[("/services/q/logs", Reply::Held(chunked(&[], false)))]).unwrap();
    let cancel = CancellationToken::new();
    let receiver = logs::service_logs(
        &config_of(&engine),
        "q",
        &ServiceLogsOptions::default(),
        Framing::Raw,
        SECS,
        1,
        cancel.clone(),
    )
    .await
    .unwrap();
    cancel.cancel();
    let (frames, end) = collect(receiver).await;
    assert!(frames.is_empty());
    assert!(matches!(end, StreamEnd::Cancelled), "{end:?}");
}

async fn collect_after(
    receiver: &mut mpsc::Receiver<LogItem>,
) -> (Vec<(Pipe, Vec<u8>)>, StreamEnd) {
    let (sender, rest) = mpsc::channel(16);
    while let Some(item) = tokio::time::timeout(SECS, receiver.recv()).await.unwrap() {
        let end = matches!(item, LogItem::End(_));
        sender.send(item).await.unwrap();
        if end {
            break;
        }
    }
    drop(sender);
    collect(rest).await
}

/// A refusal at establishment is an error of its kind, not a stream.
#[tokio::test]
async fn refusals_at_establishment_are_errors() {
    let engine =
        FakeEngine::start(&[("/services/broken/logs", Reply::error(500, "boom"))]).unwrap();
    let config = config_of(&engine);
    let cancel = CancellationToken::new();
    let error = logs::container_logs(
        &config,
        "gone",
        &every_option(),
        Framing::Raw,
        SECS,
        1,
        cancel.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
    let error = logs::service_logs(
        &config,
        "broken",
        &ServiceLogsOptions::default(),
        Framing::Raw,
        SECS,
        1,
        cancel,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.to_string().contains("boom"), "{error}");
}

/// The establishment is bounded and cancellable; the stream is not bounded by the timeout.
#[tokio::test]
async fn the_timeout_bounds_the_establishment_only() {
    let engine = FakeEngine::start(&[
        ("/containers/silent/logs", Reply::Silent),
        (
            "/containers/slow/logs",
            Reply::HeadersThenBody {
                body_delay: Duration::from_millis(400),
                body: String::from_utf8(frame(1, b"late")).unwrap(),
            },
        ),
    ])
    .unwrap();
    let config = config_of(&engine);
    let short = Duration::from_millis(200);
    let error = logs::container_logs(
        &config,
        "silent",
        &every_option(),
        Framing::Raw,
        short,
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = logs::container_logs(
        &config,
        "silent",
        &every_option(),
        Framing::Raw,
        SECS,
        1,
        cancelled,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    let receiver = logs::container_logs(
        &config,
        "slow",
        &every_option(),
        Framing::Multiplexed,
        short,
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (frames, end) = collect(receiver).await;
    assert_eq!(frames, [(Pipe::Stdout, b"late".to_vec())]);
    assert!(matches!(end, StreamEnd::Eof), "{end:?}");
}

/// Dropping the receiver ends the task, which closes the connection, even while the stream is
/// quiet.
#[tokio::test]
async fn dropping_the_receiver_closes_the_connection() {
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch =
        Scratch(std::env::temp_dir().join(format!("docker-client-logs-{}", std::process::id())));
    let dir = &scratch.0;
    std::fs::create_dir_all(dir).unwrap();
    let socket = dir.join("docker.sock");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let (closed, closed_seen) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut head = vec![0_u8; 4096];
        let _ = stream.read(&mut head).await.unwrap();
        stream
            .write_all(&chunked(&[&frame(1, b"x")], false))
            .await
            .unwrap();
        let mut rest = [0_u8; 64];
        // The client's close reads as the end of the stream.
        while stream.read(&mut rest).await.unwrap_or(0) > 0 {}
        let _ = closed.send(());
    });
    let mut receiver = logs::container_logs(
        &Config::unix(socket.to_str().unwrap()),
        "c",
        &every_option(),
        Framing::Multiplexed,
        SECS,
        1,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(matches!(receiver.recv().await, Some(LogItem::Frame(_))));
    drop(receiver);
    tokio::time::timeout(SECS, closed_seen)
        .await
        .expect("the connection closes")
        .unwrap();
}
