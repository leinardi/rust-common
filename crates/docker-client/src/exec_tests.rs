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

//! The exec calls against the fake engine.

use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::demux::{Demuxer, Framing, Pipe};
use crate::exec::{self, ExecOptions, ExecState};
use crate::test_util::{FakeEngine, Reply};
use crate::{CancellationToken, Config, ErrorKind};

const SECS: Duration = Duration::from_secs(5);

fn config_of(engine: &FakeEngine) -> Config {
    Config::unix(engine.socket().to_str().unwrap())
}

fn terminal() -> ExecOptions {
    let mut options = ExecOptions::new(vec!["/bin/sh".to_owned()]);
    options.env = vec!["TERM=xterm".to_owned(), "A=<&>".to_owned()];
    options.tty = true;
    options.attach_stdin = true;
    options.attach_stdout = true;
    options.attach_stderr = true;
    options
}

/// The four requests, their bodies and their queries are moby's Go client's: the create bodies are
/// `json.Marshal` of moby api v1.56.1's `ExecCreateRequest` printed by a Go program, `<`, `>` and
/// `&` escaped and `null` for no environment.
#[tokio::test]
async fn the_exec_requests_are_moby_s() {
    let engine = FakeEngine::start(&[
        (
            "/containers/c/exec",
            Reply::json_status(201, r#"{"Id":"e1"}"#),
        ),
        (
            "/exec/e1/start",
            Reply::Session {
                output: Vec::new(),
                echo: false,
            },
        ),
        ("/exec/e1/resize", Reply::status(201)),
        (
            "/exec/e1/json",
            Reply::ok(r#"{"Running":false,"ExitCode":3,"Pid":0}"#),
        ),
    ])
    .unwrap();
    let config = config_of(&engine)
        .user_agent("tool/1")
        .api_version("1.44")
        .unwrap();
    let cancel = CancellationToken::new();
    let id = exec::create(&config, "c", &terminal(), SECS, &cancel)
        .await
        .unwrap();
    assert_eq!(id, "e1");
    let mut background = ExecOptions::new(vec!["true".to_owned()]);
    background.attach_stdout = true;
    background.attach_stderr = true;
    exec::create(&config, "c", &background, SECS, &cancel)
        .await
        .unwrap();
    let mut stream = exec::start(&config, "e1", Framing::Raw, SECS, &cancel)
        .await
        .unwrap();
    stream.shutdown().await.unwrap();
    exec::resize(&config, "e1", 40, 120, SECS, &cancel)
        .await
        .unwrap();
    let state = exec::inspect(&config, "e1", SECS, &cancel).await.unwrap();
    assert_eq!(
        state,
        ExecState {
            running: false,
            exit_code: Some(3)
        }
    );

    let requests = engine.requests();
    let seen: Vec<(&str, &str, &[u8])> = requests
        .iter()
        .map(|request| {
            (
                request.method.as_str(),
                request.raw_path(),
                request.body.as_slice(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                "POST",
                "/v1.44/containers/c/exec",
                &br#"{"User":"","Privileged":false,"Tty":true,"AttachStdin":true,"AttachStderr":true,"AttachStdout":true,"DetachKeys":"","Env":["TERM=xterm","A=\u003c\u0026\u003e"],"WorkingDir":"","Cmd":["/bin/sh"]}"#[..]
            ),
            (
                "POST",
                "/v1.44/containers/c/exec",
                &br#"{"User":"","Privileged":false,"Tty":false,"AttachStdin":false,"AttachStderr":true,"AttachStdout":true,"DetachKeys":"","Env":null,"WorkingDir":"","Cmd":["true"]}"#[..]
            ),
            ("POST", "/v1.44/exec/e1/start", &br#"{"Detach":false,"Tty":true}"#[..]),
            ("POST", "/v1.44/exec/e1/resize?h=40&w=120", &b""[..]),
            ("GET", "/v1.44/exec/e1/json", &b""[..]),
        ]
    );
    for request in &requests {
        assert_eq!(request.header("user-agent"), Some("tool/1"), "{request:?}");
    }
    for request in &requests[..3] {
        assert_eq!(
            request.header("content-type"),
            Some("application/json"),
            "{request:?}"
        );
    }
    let start = &requests[2];
    assert_eq!(start.header("connection"), Some("Upgrade"));
    assert_eq!(start.header("upgrade"), Some("tcp"));
    assert_eq!(requests[3].header("content-type"), None);
}

/// A multiplexed start says `"Tty":false`.
#[tokio::test]
async fn a_multiplexed_start_asks_for_no_tty() {
    let engine = FakeEngine::start(&[(
        "/exec/e1/start",
        Reply::Session {
            output: Vec::new(),
            echo: false,
        },
    )])
    .unwrap();
    let cancel = CancellationToken::new();
    let mut stream = exec::start(
        &config_of(&engine),
        "e1",
        Framing::Multiplexed,
        SECS,
        &cancel,
    )
    .await
    .unwrap();
    stream.shutdown().await.unwrap();
    assert_eq!(
        engine.requests()[0].body,
        br#"{"Detach":false,"Tty":false}"#
    );
}

/// The finding-24 regression: a TTY session's output starting with `\x01`, and a prompt with no
/// newline, arrive as they are and before the client writes anything; every byte the client
/// writes, the control bytes `\x01`, `\x02` and `\x04` included, reaches the engine.
#[tokio::test]
async fn a_tty_session_is_raw_and_unbuffered_both_ways() {
    let prompt = b"\x01\x00\x00\x00\x00\x00\x00\x09/ # ".to_vec();
    let engine = FakeEngine::start(&[(
        "/exec/e1/start",
        Reply::Session {
            output: prompt.clone(),
            echo: true,
        },
    )])
    .unwrap();
    let cancel = CancellationToken::new();
    let mut stream = exec::start(&config_of(&engine), "e1", Framing::Raw, SECS, &cancel)
        .await
        .unwrap();
    let mut read = vec![0_u8; prompt.len()];
    tokio::time::timeout(SECS, stream.read_exact(&mut read))
        .await
        .expect("the prompt arrives before any input")
        .unwrap();
    assert_eq!(read, prompt);

    let input = b"echo hi\n\x01\x02\x04".to_vec();
    stream.write_all(&input).await.unwrap();
    let mut echoed = vec![0_u8; input.len()];
    tokio::time::timeout(SECS, stream.read_exact(&mut echoed))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(echoed, input);
    stream.shutdown().await.unwrap();
    let mut rest = Vec::new();
    tokio::time::timeout(SECS, stream.read_to_end(&mut rest))
        .await
        .unwrap()
        .unwrap();
    assert!(rest.is_empty(), "{rest:?}");
    assert_eq!(engine.sessions(), [input]);
}

/// Without a TTY the output is multiplexed, and a demuxer of the same framing splits it.
#[tokio::test]
async fn a_session_without_a_tty_demultiplexes() {
    let mut output = vec![1, 0, 0, 0, 0, 0, 0, 3];
    output.extend_from_slice(b"out");
    output.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 4]);
    output.extend_from_slice(b"err\n");
    let engine = FakeEngine::start(&[(
        "/exec/e1/start",
        Reply::Session {
            output,
            echo: false,
        },
    )])
    .unwrap();
    let cancel = CancellationToken::new();
    let mut options = ExecOptions::new(vec!["true".to_owned()]);
    options.attach_stdout = true;
    let framing = options.framing();
    assert_eq!(framing, Framing::Multiplexed);
    let mut stream = exec::start(&config_of(&engine), "e1", framing, SECS, &cancel)
        .await
        .unwrap();
    stream.shutdown().await.unwrap();
    let mut demuxer = Demuxer::new(framing);
    let mut frames = Vec::new();
    while let Some(frame) = demuxer.read_frame(&mut stream).await.unwrap() {
        frames.push((frame.pipe, frame.bytes.to_vec()));
    }
    assert_eq!(
        frames,
        [
            (Pipe::Stdout, b"out".to_vec()),
            (Pipe::Stderr, b"err\n".to_vec())
        ]
    );
}

/// An answer other than `101` is an error with moby's text and the status's kind.
#[tokio::test]
async fn a_start_not_upgraded_is_an_error() {
    let engine = FakeEngine::start(&[
        ("/exec/ok/start", Reply::ok("{}")),
        ("/exec/busy/start", Reply::conflict()),
    ])
    .unwrap();
    let config = config_of(&engine);
    let cancel = CancellationToken::new();
    for (id, kind, status) in [
        ("ok", ErrorKind::Other, 200),
        ("busy", ErrorKind::Conflict, 409),
        ("gone", ErrorKind::NotFound, 404),
    ] {
        let error = exec::start(&config, id, Framing::Raw, SECS, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), kind, "{error}");
        assert!(
            error
                .to_string()
                .contains(&format!("unable to upgrade to tcp, received {status}")),
            "{error}"
        );
    }
}

/// A create refused with 409 (the container is not running) keeps `Conflict`; a 404 is
/// `NotFound`; an answer without an `Id` is an error.
#[tokio::test]
async fn create_refusals_keep_their_kind() {
    let engine = FakeEngine::start(&[
        (
            "/containers/stopped/exec",
            Reply::error(409, "container is not running"),
        ),
        ("/containers/odd/exec", Reply::json_status(201, "{}")),
    ])
    .unwrap();
    let config = config_of(&engine);
    let cancel = CancellationToken::new();
    let options = terminal();
    let error = exec::create(&config, "stopped", &options, SECS, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(
        error.to_string().contains("container is not running"),
        "{error}"
    );
    let error = exec::create(&config, "gone", &options, SECS, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
    let error = exec::create(&config, "odd", &options, SECS, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
}

/// A running instance has no exit code; a mistyped field is an error.
#[tokio::test]
async fn inspect_reads_running_and_a_missing_exit_code() {
    let engine = FakeEngine::start(&[
        (
            "/exec/run/json",
            Reply::ok(r#"{"Running":true,"ExitCode":null}"#),
        ),
        ("/exec/bare/json", Reply::ok("{}")),
        ("/exec/odd/json", Reply::ok(r#"{"Running":"yes"}"#)),
    ])
    .unwrap();
    let config = config_of(&engine);
    let cancel = CancellationToken::new();
    let state = exec::inspect(&config, "run", SECS, &cancel).await.unwrap();
    assert_eq!(
        state,
        ExecState {
            running: true,
            exit_code: None
        }
    );
    let state = exec::inspect(&config, "bare", SECS, &cancel).await.unwrap();
    assert_eq!(
        state,
        ExecState {
            running: false,
            exit_code: None
        }
    );
    let error = exec::inspect(&config, "odd", SECS, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    let error = exec::inspect(&config, "gone", SECS, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

/// The establishment of each call is bounded by the timeout and ended by the token, a stalled body
/// included for the complete calls; the session itself is not bounded.
#[tokio::test]
async fn timeout_and_cancel_end_the_establishment_not_the_session() {
    let engine = FakeEngine::start(&[
        ("/exec/silent/start", Reply::Silent),
        ("/exec/silent/json", Reply::Stalled),
        (
            "/exec/slow/start",
            Reply::Delayed {
                delay: Duration::from_millis(300),
                reply: Box::new(Reply::Session {
                    output: b"late".to_vec(),
                    echo: false,
                }),
            },
        ),
    ])
    .unwrap();
    let config = config_of(&engine);
    let short = Duration::from_millis(200);
    let cancel = CancellationToken::new();
    let error = exec::start(&config, "silent", Framing::Raw, short, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    let error = exec::inspect(&config, "silent", short, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);

    let during = CancellationToken::new();
    let canceller = {
        let during = during.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            during.cancel();
        })
    };
    let begun = tokio::time::Instant::now();
    let error = exec::start(&config, "silent", Framing::Raw, SECS, &during)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert!(
        begun.elapsed() < Duration::from_secs(2),
        "{:?}",
        begun.elapsed()
    );
    canceller.await.unwrap();
    let error = exec::create(&config, "c", &terminal(), SECS, &during)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled);

    // Established within its bound, the session outlives it: input written after the bound has
    // passed still reaches the engine.
    let mut stream = exec::start(
        &config,
        "slow",
        Framing::Raw,
        Duration::from_secs(1),
        &cancel,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let mut late = [0_u8; 4];
    stream.read_exact(&mut late).await.unwrap();
    assert_eq!(&late, b"late");
    stream.write_all(b"after").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).await.unwrap();
    assert_eq!(engine.sessions(), [b"after".to_vec()]);
}

/// Output the engine sends in the same write as its `101` (read ahead by hyper with the
/// headers) is returned first and whole.
#[tokio::test]
async fn output_sent_with_the_101_is_not_lost() {
    let engine =
        FakeEngine::start(&[("/exec/e1/start", Reply::upgrade(b"\x01out\x02\n"))]).unwrap();
    let cancel = CancellationToken::new();
    let mut stream = exec::start(&config_of(&engine), "e1", Framing::Raw, SECS, &cancel)
        .await
        .unwrap();
    let mut read = Vec::new();
    tokio::time::timeout(SECS, stream.read_to_end(&mut read))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read, b"\x01out\x02\n");
}

/// An empty or blank ID is refused before anything is sent, with moby's text; a padded one is
/// trimmed.
#[tokio::test]
async fn create_and_resize_trim_the_id_as_moby_does() {
    let engine = FakeEngine::start(&[
        (
            "/containers/c/exec",
            Reply::json_status(201, r#"{"Id":"e1"}"#),
        ),
        ("/exec/e1/resize", Reply::status(200)),
    ])
    .unwrap();
    let config = config_of(&engine);
    let cancel = CancellationToken::new();
    let error = exec::create(&config, " ", &terminal(), SECS, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(
        error
            .to_string()
            .contains("invalid container name or ID: value is empty"),
        "{error}"
    );
    let error = exec::resize(&config, "", 1, 1, SECS, &cancel)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid exec name or ID: value is empty"),
        "{error}"
    );
    assert!(engine.requests().is_empty());
    exec::create(&config, " c\n", &terminal(), SECS, &cancel)
        .await
        .unwrap();
    exec::resize(&config, "\te1 ", 1, 1, SECS, &cancel)
        .await
        .unwrap();
    let paths: Vec<String> = engine
        .requests()
        .iter()
        .map(|request| request.raw_path().to_owned())
        .collect();
    assert_eq!(paths, ["/containers/c/exec", "/exec/e1/resize?h=1&w=1"]);
}

/// Every exec call escapes its ID in the path as moby's client does.
#[tokio::test]
async fn every_exec_call_escapes_the_id() {
    let engine = FakeEngine::start(&[]).unwrap();
    let config = config_of(&engine);
    let cancel = CancellationToken::new();
    assert!(
        exec::create(&config, "a?b", &terminal(), SECS, &cancel)
            .await
            .is_err()
    );
    assert!(
        exec::start(&config, "a?b", Framing::Raw, SECS, &cancel)
            .await
            .is_err()
    );
    assert!(
        exec::resize(&config, "a?b", 24, 80, SECS, &cancel)
            .await
            .is_err()
    );
    assert!(exec::inspect(&config, "a?b", SECS, &cancel).await.is_err());
    let paths: Vec<String> = engine
        .requests()
        .iter()
        .map(|request| request.path().to_owned())
        .collect();
    assert_eq!(
        paths,
        [
            "/containers/a%3Fb/exec",
            "/exec/a%3Fb/start",
            "/exec/a%3Fb/resize",
            "/exec/a%3Fb/json",
        ]
    );
}
