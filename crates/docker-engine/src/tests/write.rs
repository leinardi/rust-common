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

//! The writes (`write`) and the attach (`attach`), as swarm-device-access's launcher sends them:
//! the request a write's shape makes, the write-safe retry, the hijacked attach demultiplexed by
//! moby's `StdCopy`, and the wait's answer.
//!
//! Extracted from swarm-device-access/src/docker/tests.rs at 8b4ab3a3e417
//! (`a_write_is_not_resent_on_a_dropped_connection`).

use bytes::Bytes;
use http_body_util::Full;
use hyper::Method;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};

use super::{FakeDaemon, Reply, engine, list_containers, ok, respond};
use crate::ErrorKind;

/// A write whose reused connection drops after the request went out is not sent again: the
/// daemon may have carried it out, and net/http resends a request that is not replayable only
/// when nothing of it was written.
#[tokio::test]
async fn a_write_is_not_resent_on_a_dropped_connection() {
    let daemon = FakeDaemon::unix(|request| {
        if request.target.ends_with("/start") {
            Reply::Close
        } else {
            engine(Some("1.56"))(request)
        }
    });
    let client = daemon.client();
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    let started = client
        .exchange("/containers/c/start", "", |request| {
            *request.method_mut() = Method::POST;
        })
        .await;
    assert!(started.is_err());
    let requests = daemon.requests();
    assert_eq!(
        daemon.wire(),
        [
            "HEAD /_ping",
            "GET /v1.56/containers/json",
            "POST /v1.56/containers/c/start"
        ]
    );
    // It went out on the reused connection, where a GET would have been sent again.
    assert_eq!(requests[1].connection, requests[2].connection);
}

/// A write's shape sets its method, headers and body on the one builder's request; a daemon's
/// refusal is moby's response error.
#[tokio::test]
async fn an_exchange_sends_the_shaped_request() {
    let daemon = FakeDaemon::unix(|request| match request.target.as_str() {
        "/_ping" => ok(&[("Api-Version", "1.56")], "OK"),
        "/v1.56/containers/create?name=d" => respond(
            201,
            &[("Content-Type", "application/json")],
            r#"{"Id":"c1","Warnings":[]}"#,
        ),
        _ => respond(
            409,
            &[("Content-Type", "application/json")],
            r#"{"message":"conflict"}"#,
        ),
    });
    let client = daemon.client();
    let body = Bytes::from_static(br#"{"Image":"x"}"#);
    let response = client
        .exchange("/containers/create", "name=d", |request| {
            *request.method_mut() = Method::POST;
            *request.body_mut() = Full::new(body.clone());
            request
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        })
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    crate::ensure_reader_closed(response).await;
    let error = client
        .exchange("/containers/c1/start", "", |request| {
            *request.method_mut() = Method::POST;
            request
                .headers_mut()
                .insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Response);
    assert_eq!(error.to_string(), "Error response from daemon: conflict");
    let requests = daemon.requests();
    let user_agent = crate::moby_user_agent();
    assert_eq!(
        requests[1].head,
        format!(
            "POST /v1.56/containers/create?name=d HTTP/1.1\r\nHost: api.moby.localhost\r\n\
             User-Agent: {user_agent}\r\nContent-Type: application/json\r\nContent-Length: 13"
        )
    );
    assert_eq!(requests[1].body, br#"{"Image":"x"}"#);
    assert_eq!(
        requests[2].head,
        format!(
            "POST /v1.56/containers/c1/start HTTP/1.1\r\nHost: api.moby.localhost\r\n\
             User-Agent: {user_agent}\r\nContent-Length: 0"
        )
    );
    // The create's short answer was drained, so the start reused its connection.
    assert_eq!(requests[1].connection, requests[2].connection);
}

#[cfg(feature = "attach")]
mod attach {
    use hyper::Method;
    use hyper::header::{CONNECTION, HeaderValue, UPGRADE};
    use hyper_util::rt::TokioIo;

    use super::super::{FakeDaemon, Reply, ok, respond};
    use crate::ErrorKind;
    use crate::stdcopy::std_copy;
    use crate::wait::{WaitExitError, WaitResponse, wait_answer};

    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![stream, 0, 0, 0];
        out.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn upgrade(request: &mut hyper::Request<http_body_util::Full<bytes::Bytes>>) {
        *request.method_mut() = Method::POST;
        let headers = request.headers_mut();
        headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(UPGRADE, HeaderValue::from_static("tcp"));
    }

    /// moby's hijacked attach: the `101` hands the connection over, and its bytes are the
    /// multiplexed output `StdCopy` splits.
    #[tokio::test]
    async fn an_attach_is_upgraded_and_demultiplexed() {
        let output = [frame(1, b"out\n"), frame(2, b"err\n")].concat();
        let daemon = FakeDaemon::unix(move |request| {
            if request.target == "/_ping" {
                return ok(&[("Api-Version", "1.56")], "OK");
            }
            let mut raw = b"HTTP/1.1 101 UPGRADED\r\nContent-Type: \
                            application/vnd.docker.raw-stream\r\nConnection: Upgrade\r\n\
                            Upgrade: tcp\r\n\r\n"
                .to_vec();
            raw.extend_from_slice(&output);
            Reply::Raw(raw)
        });
        let client = daemon.client();
        let upgraded = client
            .exchange_upgrade(
                "/containers/c1/attach",
                "stderr=1&stdout=1&stream=1",
                upgrade,
            )
            .await
            .unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut source = TokioIo::new(upgraded);
        let copied = std_copy(&mut stdout, &mut stderr, &mut source).await;
        assert_eq!(copied, Ok(8));
        assert_eq!(stdout, b"out\n");
        assert_eq!(stderr, b"err\n");
        let requests = daemon.requests();
        assert_eq!(
            daemon.wire(),
            [
                "HEAD /_ping",
                "POST /v1.56/containers/c1/attach?stderr=1&stdout=1&stream=1"
            ]
        );
        // A connection of its own, never the pool's.
        assert_ne!(requests[0].connection, requests[1].connection);
    }

    /// On TCP too, the hijacked request carries no `Accept-Encoding`: moby writes it on the
    /// connection itself (`req.Write`), not through the transport that adds `gzip` to a pooled
    /// TCP request (`tcp_requests_match_go_s_wire`).
    #[cfg(feature = "tcp")]
    #[tokio::test]
    async fn a_tcp_attach_sends_no_accept_encoding() {
        let daemon = FakeDaemon::tcp(|request| {
            if request.target == "/_ping" {
                return ok(&[("Api-Version", "1.56")], "OK");
            }
            Reply::Raw(
                b"HTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n".to_vec(),
            )
        });
        let client = daemon.client();
        let upgraded = client
            .exchange_upgrade("/containers/c1/attach", "stream=1", upgrade)
            .await
            .unwrap();
        drop(upgraded);
        let requests = daemon.requests();
        assert_eq!(
            daemon.wire(),
            ["HEAD /_ping", "POST /v1.56/containers/c1/attach?stream=1"]
        );
        let user_agent = crate::moby_user_agent();
        assert_eq!(
            requests[1].head,
            format!(
                "POST /v1.56/containers/c1/attach?stream=1 HTTP/1.1\r\nHost: {}\r\nUser-Agent: \
                 {user_agent}\r\nConnection: Upgrade\r\nUpgrade: tcp",
                daemon.host.strip_prefix("tcp://").unwrap()
            )
        );
        assert!(requests[1].header("accept-encoding").is_empty());
    }

    /// An answer that is not an upgrade is moby's `setupHijackConn` error.
    #[tokio::test]
    async fn an_attach_that_is_not_upgraded_fails_as_moby() {
        let daemon = FakeDaemon::unix(|request| match request.target.as_str() {
            "/_ping" => ok(&[("Api-Version", "1.56")], "OK"),
            _ => respond(200, &[], ""),
        });
        let error = daemon
            .client()
            .exchange_upgrade("/containers/c1/attach", "", upgrade)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
        assert_eq!(error.to_string(), "unable to upgrade to tcp, received 200");
    }

    /// The wait's answer, read from a chunked body as moby's wait goroutine reads it; an answer
    /// that is not JSON is reported by its text.
    #[tokio::test]
    async fn a_wait_answer_is_read_as_moby_reads_it() {
        let daemon = FakeDaemon::unix(|request| {
            match request.target.as_str() {
            "/_ping" => ok(&[("Api-Version", "1.56")], "OK"),
            "/v1.56/containers/c1/wait?condition=removed" => Reply::Raw(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: \
                  chunked\r\n\r\n10\r\n{\"StatusCode\":3,\r\n1c\r\n\"Error\":{\"Message\":\"oops\"}}\n\r\n0\r\n\r\n"
                    .to_vec(),
            ),
            _ => Reply::Raw(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\nproxy error\r\n0\r\n\r\n"
                    .to_vec(),
            ),
        }
        });
        let client = daemon.client();
        let post = |request: &mut hyper::Request<http_body_util::Full<bytes::Bytes>>| {
            *request.method_mut() = Method::POST;
        };
        let response = client
            .exchange("/containers/c1/wait", "condition=removed", post)
            .await
            .unwrap();
        let answer: WaitResponse = wait_answer(response.into_body()).await.unwrap();
        assert_eq!(
            answer,
            WaitResponse {
                status_code: 3,
                error: Some(WaitExitError {
                    message: "oops".to_owned()
                }),
            }
        );
        let response = client
            .exchange("/containers/c2/wait", "condition=next-exit", post)
            .await
            .unwrap();
        let error = wait_answer::<WaitResponse>(response.into_body())
            .await
            .unwrap_err();
        assert_eq!(error, "proxy error");
    }
}
