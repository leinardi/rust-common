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

//! The wire-capture harness ([`crate::testing`]) on Go 1.26 captures of the mux's own answers,
//! the ones that depend on no program's routes: a 404, a 307 for `GET` and `POST`, `OPTIONS *`,
//! `CONNECT`, `HEAD` and two pipelined requests. They are copied from
//! swarm-scheduler-exporter/tests/testdata/http at c9e79afa8810 (port 18080 is its healthy
//! server, whose `/healthz` answers `ok` with its type set), and from
//! swarm-device-access/tests/testdata/http at 8b4ab3a3e417 for the sniffed `/readyz` (its own port
//! 18080); each program keeps its whole set.
//!
//! Extracted from swarm-scheduler-exporter/src/server/tests/wire.rs at c9e79afa8810.

use super::*;
use crate::testing::{self, Capture, Replay};

const GET_NOPE: &[u8] = b"### get-nope  (port 18080)
### request: GET /nope HTTP/1.1\\r\\nHost: x\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 404 Not Found\r
Content-Type: text/plain; charset=utf-8\r
X-Content-Type-Options: nosniff\r
Date: Sun, 25 Oct 2026 08:44:07 GMT\r
Content-Length: 19\r
Connection: close\r
\r
404 page not found
[end: <nil>]

";

const GET_DSLASH_METRICS: &[u8] = b"### get-dslash-metrics  (port 18080)
### request: GET //metrics HTTP/1.1\\r\\nHost: x\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 307 Temporary Redirect\r
Content-Type: text/html; charset=utf-8\r
Location: /metrics\r
Date: Sun, 25 Oct 2026 08:44:07 GMT\r
Content-Length: 44\r
Connection: close\r
\r
<a href=\"/metrics\">Temporary Redirect</a>.

[end: <nil>]

";

const POST_DSLASH: &[u8] = b"### post-dslash  (port 18080)
### request: POST //healthz HTTP/1.1\\r\\nHost: x\\r\\nContent-Length: 0\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 307 Temporary Redirect\r
Location: /healthz\r
Date: Sun, 25 Oct 2026 08:44:07 GMT\r
Content-Length: 0\r
Connection: close\r
\r
[end: <nil>]

";

const GET_STAR: &[u8] = b"### get-star  (port 18080)
### request: OPTIONS * HTTP/1.1\\r\\nHost: x\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 200 OK\r
Content-Length: 0\r
Date: Sun, 25 Oct 2026 08:44:07 GMT\r
Connection: close\r
\r
[end: <nil>]

";

const CONNECT: &[u8] = b"### connect  (port 18080)
### request: CONNECT x:80 HTTP/1.1\\r\\nHost: x\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 404 Not Found\r
Content-Type: text/plain; charset=utf-8\r
X-Content-Type-Options: nosniff\r
Date: Sun, 25 Oct 2026 08:44:41 GMT\r
Content-Length: 19\r
Connection: close\r
\r
404 page not found
[end: <nil>]

";

const HEAD_NOPE: &[u8] = b"### head-nope  (port 18080)
### request: HEAD /nope HTTP/1.1\\r\\nHost: x\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 404 Not Found\r
Content-Type: text/plain; charset=utf-8\r
X-Content-Type-Options: nosniff\r
Date: Sun, 25 Oct 2026 08:44:07 GMT\r
Content-Length: 19\r
Connection: close\r
\r
[end: <nil>]

";

const KEEPALIVE_TWO: &[u8] = b"### keepalive-two  (port 18080)
### request: GET /healthz HTTP/1.1\\r\\nHost: x\\r\\n\\r\\nGET /nope HTTP/1.1\\r\\nHost: x\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 200 OK\r
Content-Type: text/plain; charset=utf-8\r
Date: Sun, 25 Oct 2026 08:44:07 GMT\r
Content-Length: 3\r
\r
ok
HTTP/1.1 404 Not Found\r
Content-Type: text/plain; charset=utf-8\r
X-Content-Type-Options: nosniff\r
Date: Sun, 25 Oct 2026 08:44:07 GMT\r
Content-Length: 19\r
Connection: close\r
\r
404 page not found
[end: <nil>]

";

/// swarm-device-access's sniffed `/readyz`.
const GET_READYZ_OK: &[u8] = b"### get-readyz-ok  (port 18080)
### request: GET /readyz HTTP/1.1\\r\\nHost: x\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 200 OK\r
Date: Tue, 27 Oct 2026 15:18:56 GMT\r
Content-Length: 2\r
Content-Type: text/plain; charset=utf-8\r
Connection: close\r
\r
ok[end: <nil>]

";

/// A `/metrics` answer cut at its head, as a capture of a long body is.
const GET_METRICS_CUT: &[u8] = b"### get-metrics  (port 18080)
### request: GET /metrics HTTP/1.1\\r\\nHost: x\\r\\nConnection: close\\r\\n\\r\\n
HTTP/1.1 200 OK\r
Content-Type: text/plain; version=0.0.4; charset=utf-8; escaping=underscores\r
Date: Sun, 25 Oct 2026 08:44:07 GMT\r
Connection: close\r
Transfer-Encoding: chunked\r
\r
800\r
# HELP
";

fn captures() -> Vec<Capture> {
    [
        ("get-nope", GET_NOPE),
        ("get-dslash-metrics", GET_DSLASH_METRICS),
        ("post-dslash", POST_DSLASH),
        ("get-star", GET_STAR),
        ("connect", CONNECT),
        ("head-nope", HEAD_NOPE),
        ("keepalive-two", KEEPALIVE_TWO),
        ("get-readyz-ok", GET_READYZ_OK),
        ("get-metrics", GET_METRICS_CUT),
    ]
    .into_iter()
    .map(|(name, text)| Capture::parse(name, text).unwrap())
    .collect()
}

/// The test server at the capture's time; `/metrics` answers 3300 bytes, more than Go buffers
/// before chunking.
fn server_for(capture: &Capture) -> Server {
    assert_eq!(capture.port, 18080, "{}", capture.name);
    let body = "# HELP x y\n".repeat(300).leak().as_bytes();
    server_with(
        text_metrics(body),
        sleeping(Duration::ZERO),
        capture.date().unwrap(),
    )
}

/// Every capture is reproduced byte for byte, `Date` included.
#[tokio::test(start_paused = true)]
async fn the_go_captures_of_the_mux_are_reproduced() {
    let captures = captures();
    let replay = testing::replay(&captures, EXPORTER, server_for).await;
    assert_eq!(replay.different, Vec::<String>::new(), "{}", replay.report);
    assert_eq!(replay.identical.len(), 9);
}

/// The control: a server whose clock is a second off differs on every capture with a `Date`,
/// and the report shows both answers.
#[tokio::test(start_paused = true)]
async fn a_wrong_answer_is_reported() {
    let captures = captures();
    let Replay {
        identical,
        different,
        report,
    } = testing::replay(&captures, EXPORTER, |capture| {
        let late = capture.date().unwrap() + jiff::SignedDuration::from_secs(1);
        server_with(text_metrics(b""), sleeping(Duration::ZERO), late)
    })
    .await;
    assert!(identical.is_empty(), "{identical:?}");
    assert_eq!(different.len(), 9);
    assert!(
        report.contains("get-nope differs:\n--- Go\nHTTP/1.1 404 Not Found\r\n"),
        "{report}"
    );
    assert!(
        report.contains("Date: Sun, 25 Oct 2026 08:44:08 GMT"),
        "{report}"
    );
}

/// A capture is parsed as the Go capture tool wrote it; a file that is not one is refused.
#[test]
fn captures_parse() {
    let nope = Capture::parse("get-nope", GET_NOPE).unwrap();
    assert_eq!(nope.port, 18080);
    assert_eq!(
        nope.request,
        b"GET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    );
    assert!(nope.response.ends_with(b"\r\n\r\n404 page not found\n"));
    assert!(!nope.truncated);
    assert_eq!(nope.date(), Some(capture_time()));
    let cut = Capture::parse("get-metrics", GET_METRICS_CUT).unwrap();
    assert!(cut.truncated);
    assert!(cut.response.ends_with(b"800\r\n# HELP"));
    assert!(cut.matches(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8; escaping=underscores\r\nDate: Sun, 25 Oct 2026 08:44:07 GMT\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n800\r\n# HELP x y\n"));
    assert_eq!(
        testing::head(&cut.response),
        Some(
            &b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8; \
               escaping=underscores\r\nDate: Sun, 25 Oct 2026 08:44:07 GMT\r\nConnection: \
               close\r\nTransfer-Encoding: chunked"[..]
        )
    );
    assert!(Capture::parse("x", b"### no port\n### request: x\n").is_err());
    assert!(Capture::parse("x", b"### (port 1)\nGET /\n").is_err());
    assert!(Capture::parse("x", b"### (port 1)\n### request: x").is_err());
}

/// Every capture in a directory, sorted by name; an unreadable directory is an error.
#[test]
fn captures_are_read_from_a_directory() {
    let directory = std::env::temp_dir().join(format!("go-http-captures-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("b-nope.txt"), GET_NOPE).unwrap();
    std::fs::write(directory.join("a-star.txt"), GET_STAR).unwrap();
    let read = testing::read_captures(&directory);
    std::fs::remove_dir_all(&directory).unwrap();
    let names: Vec<String> = read
        .unwrap()
        .into_iter()
        .map(|capture| capture.name)
        .collect();
    assert_eq!(names, ["a-star", "b-nope"]);
    assert!(testing::read_captures(&directory).is_err());
}
