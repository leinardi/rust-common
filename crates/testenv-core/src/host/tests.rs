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

//! The sweep and the cleanup against a fake daemon on a unix socket: what is listed, what is
//! removed with which options, what is logged, and the errors. No Docker needed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::{API_DEFAULT_VERSION, Docker};
use go_clock::FakeClock;
use go_slog::MemorySink;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;

use super::{HostSweep, OP_TIMEOUT, OpError, process_alive, sweep_rules, with_timeout};
use crate::context::{Context, Ended};
use crate::redact::RedactingLogger;
use crate::sweep_rules::{Labels, Owner, Resources, current_pid_namespace, hostname};

const LABELS: Labels = Labels {
    env_id: "it.envid",
    owner: "it.owner",
    started: "it.started",
    pid_namespace: "it.pidns",
};

/// swarm-scheduler-exporter's shape: containers with their volumes, and networks.
const SSE: HostSweep = HostSweep {
    labels: LABELS,
    resources: Resources {
        remove_volumes: true,
        networks: true,
        images: false,
    },
    op_timeout: OP_TIMEOUT,
};

/// swarm-device-access's shape: containers and images.
const SDA: HostSweep = HostSweep {
    labels: LABELS,
    resources: Resources {
        remove_volumes: false,
        networks: false,
        images: true,
    },
    op_timeout: OP_TIMEOUT,
};

/// `METHOD decoded-target` of every request the fake daemon answered.
type Wire = Arc<Mutex<Vec<String>>>;

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() + 1 {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap();
            out.push(u8::from_str_radix(hex, 16).unwrap());
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// The answer to one request: a status and a JSON body.
fn answer(method: &str, target: &str, labels: &str) -> (u16, String) {
    let path = target.split('?').next().unwrap_or_default();
    let selects_old = target.contains("it.envid=old");
    match (method, path) {
        ("GET", "/containers/json") if selects_old => (
            200,
            r#"[{"Id":"c-old"},{"Id":"c-gone"},{"Id":"c-fail"}]"#.to_owned(),
        ),
        ("GET", "/containers/json") => (200, format!("[{labels}]")),
        ("GET", "/networks") if selects_old => (200, r#"[{"Id":"n-old"}]"#.to_owned()),
        ("GET", "/images/json") if selects_old => (
            200,
            concat!(
                r#"[{"Id":"i-old","ParentId":"","RepoTags":[],"RepoDigests":[],"Created":0,"#,
                r#""Size":0,"SharedSize":0,"Labels":{},"Containers":0}]"#
            )
            .to_owned(),
        ),
        ("DELETE", "/containers/c-gone") => {
            (404, r#"{"message":"No such container: c-gone"}"#.to_owned())
        }
        ("DELETE", "/containers/c-fail") => (500, r#"{"message":"device busy"}"#.to_owned()),
        ("GET", "/networks" | "/images/json") | ("DELETE", "/images/i-old") => {
            (200, "[]".to_owned())
        }
        ("DELETE", _) => (204, String::new()),
        _ => (404, r#"{"message":"page not found"}"#.to_owned()),
    }
}

/// A fake daemon serving `labels` (the JSON of the containers' listing) on a fresh socket.
fn daemon(labels: String) -> (Docker, Wire, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("docker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let wire: Wire = Arc::default();
    let recorded = Arc::clone(&wire);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let recorded = Arc::clone(&recorded);
            let labels = labels.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                loop {
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if header == "\r\n" {
                            break;
                        }
                    }
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_owned();
                    let target = percent_decode(parts.next().unwrap_or_default());
                    recorded.lock().unwrap().push(format!("{method} {target}"));
                    let (status, body) = answer(&method, &target, &labels);
                    let head = if status == 204 {
                        "HTTP/1.1 204 No Content\r\n\r\n".to_owned()
                    } else {
                        format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\n\r\n",
                            body.len()
                        )
                    };
                    let reply = format!("{head}{body}");
                    if write.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let client =
        Docker::connect_with_unix(&socket.to_string_lossy(), 120, API_DEFAULT_VERSION).unwrap();
    (client, wire, dir)
}

/// A PID that names no process: a child that has exited and been reaped.
fn dead_pid() -> i64 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = i64::from(child.id());
    child.wait().unwrap();
    pid
}

fn labels_json(env_id: &str, owner: &Owner, started: &str) -> String {
    format!(
        concat!(
            r#"{{"Id":"x-{env_id}","Labels":{{"it.envid":"{env_id}","it.owner":"{owner}","#,
            r#""it.started":"{started}","it.pidns":"{pid_namespace}"}}}}"#
        ),
        env_id = env_id,
        owner = owner,
        started = started,
        pid_namespace = owner.pid_namespace,
    )
}

fn logger(sink: &MemorySink) -> RedactingLogger {
    let at: Timestamp = "2026-10-11T12:00:00Z".parse().unwrap();
    RedactingLogger::new(
        Box::new(sink.clone()),
        TimeZone::UTC,
        Arc::new(FakeClock::new(at)),
    )
}

/// The decisions on three runs: an abandoned one is removed (its containers with their volumes,
/// then its networks), a live one and the current one are kept, and a removal that fails is
/// logged with the others still made.
#[tokio::test]
async fn the_sweep_removes_only_the_abandoned_run() {
    let pid_namespace = current_pid_namespace();
    let host = hostname().unwrap();
    let me = Owner {
        host: host.clone(),
        pid: i64::from(std::process::id()),
        pid_namespace: pid_namespace.clone(),
    };
    let gone = Owner {
        pid: dead_pid(),
        ..me.clone()
    };
    let old = "2000-01-01T00:00:00Z";
    let listing = [
        labels_json("old", &gone, old),
        labels_json("live", &me, old),
        labels_json("self", &me, old),
    ]
    .join(",");
    let (client, wire, _dir) = daemon(listing);
    let sink = MemorySink::default();
    let rules = sweep_rules(Timestamp::now(), Duration::from_secs(3600), "self", &me);
    SSE.sweep(&Context::background(), &client, &rules, &logger(&sink))
        .await;

    let wire: Vec<String> = wire.lock().unwrap().clone();
    let deletes: Vec<&String> = wire
        .iter()
        .filter(|line| line.starts_with("DELETE"))
        .collect();
    assert_eq!(
        deletes,
        [
            "DELETE /containers/c-old?v=true&force=true&link=false",
            "DELETE /containers/c-gone?v=true&force=true&link=false",
            "DELETE /containers/c-fail?v=true&force=true&link=false",
            "DELETE /networks/n-old",
        ],
        "{wire:#?}"
    );
    let log = sink.contents();
    let lines: BTreeMap<&str, &str> = log
        .lines()
        .map(|line| {
            let message = line.split(" msg=").nth(1).unwrap_or_default();
            (message.split(' ').next().unwrap_or_default(), line)
        })
        .collect();
    assert!(
        log.contains(&format!(
            "level=DEBUG msg=\"sweep: keeping environment\" other_envid=live \
             reason=\"its owner, PID {}, is still running\"",
            me.pid
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "level=INFO msg=\"sweep: removing leaked environment\" leaked_envid=old \
             reason=\"stale, and its owner PID {} is gone\"",
            gone.pid
        )),
        "{log}"
    );
    assert!(
        log.contains(
            "level=WARN msg=\"sweep: cleanup incomplete\" leaked_envid=old err=\"remove container \
             c-fail: "
        ),
        "{log}"
    );
    // The current run is never logged, and nothing else is.
    assert!(!log.contains("other_envid=self"), "{log}");
    assert_eq!(log.lines().count(), 3, "{lines:?}");
}

/// swarm-device-access's shape removes images rather than networks, and containers without their
/// volumes; a container already gone is not an error, a failed removal is.
#[tokio::test]
async fn the_cleanup_removes_what_the_suite_leaves() {
    let (client, wire, _dir) = daemon(String::new());
    let error = SDA
        .cleanup(&Context::background(), &client, "old")
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.starts_with("remove container c-fail: "),
        "{message}"
    );
    assert!(message.contains("device busy"), "{message}");
    assert_eq!(message.lines().count(), 1, "{message}");
    let wire: Vec<String> = wire.lock().unwrap().clone();
    let deletes: Vec<&String> = wire
        .iter()
        .filter(|line| line.starts_with("DELETE"))
        .collect();
    assert_eq!(
        deletes,
        [
            "DELETE /containers/c-old?v=false&force=true&link=false",
            "DELETE /containers/c-gone?v=false&force=true&link=false",
            "DELETE /containers/c-fail?v=false&force=true&link=false",
            "DELETE /images/i-old?force=true&noprune=false",
        ],
        "{wire:#?}"
    );
    assert!(
        wire.iter()
            .any(|line| line.starts_with("GET /images/json") && line.contains("it.envid=old")),
        "{wire:#?}"
    );
    assert!(
        !wire.iter().any(|line| line.contains("/networks")),
        "{wire:#?}"
    );
}

/// With no daemon listening, every listing fails and is reported, and the sweep only logs.
#[tokio::test]
async fn a_missing_daemon_fails_the_listings() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("closed.sock");
    // A socket nobody listens on any more: every connection is refused.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    let client =
        Docker::connect_with_unix(&socket.to_string_lossy(), 120, API_DEFAULT_VERSION).unwrap();
    let error = SSE
        .cleanup(&Context::background(), &client, "old")
        .await
        .unwrap_err()
        .to_string();
    let lines: Vec<&str> = error.lines().collect();
    assert_eq!(lines.len(), 2, "{error}");
    assert!(lines[0].starts_with("list containers: "), "{error}");
    assert!(lines[1].starts_with("list networks: "), "{error}");

    let sink = MemorySink::default();
    let me = Owner {
        host: "here".to_owned(),
        pid: 1,
        pid_namespace: String::new(),
    };
    let rules = sweep_rules(Timestamp::now(), Duration::from_secs(1), "self", &me);
    SDA.sweep(&Context::background(), &client, &rules, &logger(&sink))
        .await;
    let log = sink.contents();
    assert!(
        log.contains("level=WARN msg=\"sweep: list containers\" err="),
        "{log}"
    );
    assert!(
        log.contains("level=WARN msg=\"sweep: list images\" err="),
        "{log}"
    );
    assert_eq!(log.lines().count(), 2, "{log}");
}

/// This process is alive; a reaped child, PID 0, a negative PID and one beyond `pid_t` are not.
/// (`parse_owner` refuses the last three before they get here.)
#[test]
fn process_liveness() {
    assert!(process_alive(i64::from(std::process::id())));
    assert!(!process_alive(dead_pid()));
    assert!(!process_alive(0));
    assert!(!process_alive(-1));
    assert!(!process_alive(i64::from(i32::MAX) + 1));
}

/// A call bounded by a context that has ended fails with Go's words, and a 404 is "not found".
#[tokio::test(start_paused = true)]
async fn a_bounded_call_ends_with_its_context() {
    let ctx = Context::background();
    let error = with_timeout(
        &ctx,
        Duration::from_secs(1),
        std::future::pending::<Result<(), bollard::errors::Error>>(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, OpError::Ended(Ended::DeadlineExceeded)));
    assert_eq!(error.to_string(), "context deadline exceeded");
    assert!(!error.is_not_found());
    let not_found = OpError::from(bollard::errors::Error::DockerResponseServerError {
        status_code: 404,
        message: "gone".to_owned(),
    });
    assert!(not_found.is_not_found());
    let ok: Result<u8, OpError> = with_timeout(&ctx, Duration::from_secs(1), async {
        Ok::<_, bollard::errors::Error>(7)
    })
    .await;
    assert_eq!(ok.unwrap(), 7);
}
