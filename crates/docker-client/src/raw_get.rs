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

//! Reads answered as the engine's bytes, for a caller that decodes them itself: bollard's models
//! close every enum (a task state, a container state), so one value the model does not know fails
//! the whole answer, where moby's Go client decodes those fields as plain strings and takes any.
//!
//! A read goes out as bollard's own calls do, over the same endpoint the [`Config`] describes,
//! with the same `User-Agent` and the API version prefix the client sends ([`negotiate`]'s, once
//! it ran). The caller builds the path and the query: an ID goes into the path through
//! [`escape_id`], as moby's client escapes it, and the query is sent as given.
//!
//! [`ping`] is moby's ping, which its client negotiates the API version with: the version the
//! engine offers is in a header, which bollard's `version` call does not read.
//!
//! [`negotiate`]: crate::negotiate

use std::time::Duration;

use http::{Method, StatusCode};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::stream::{Answer, Call, Target, answered, complete, refusal_of};

pub use crate::stream::escape_id;

/// `GET <path>?<query>` (no `?` for an empty `query`), and the whole body of the engine's answer.
/// `path` is the path without the version prefix (`/services`, `/containers/<id>/json`), and
/// `query` the encoded query string without its `?`. `timeout` bounds the whole call, the body
/// included; `cancel` ends it at once.
///
/// # Errors
///
/// The engine's refusal sorted into its kind (404 is `NotFound`, 409 `Conflict`, the rest
/// `Other`), with the engine's message; the bound's `Timeout`, `Cancelled`, and `Unavailable` when
/// the engine cannot be reached or the connection breaks; `Other` for an answer past `config`'s
/// [`response_body`](crate::Limits::response_body), a refusal that long keeping its kind.
pub async fn get(
    config: &Config,
    path: &str,
    query: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, Error> {
    let body = complete(
        config,
        &Target::of(config),
        Call::new(Method::GET, path, query),
        timeout,
        cancel,
    )
    .await?;
    Ok(Vec::from(body))
}

/// What the engine's `/_ping` answer says, as moby's client reads it (`PingResult`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Ping {
    /// The `Api-Version` header: the newest API the engine speaks, empty when it sent none.
    pub api_version: String,
    /// The `Ostype` header, empty when it sent none.
    pub os_type: String,
}

/// moby's ping: `HEAD /_ping` without the version prefix, then `GET /_ping` when the `HEAD` fails
/// or answers anything but `200` (a `HEAD` answer has no body to read a refusal from). `timeout`
/// bounds both; `cancel` ends them at once.
///
/// # Errors
///
/// The `GET`'s failure to connect, its bound or cancellation, or, for an answer of 400 or more,
/// the engine's refusal sorted into its kind with the engine's message. Only the headers of a
/// success are used, so a success past `config`'s [`response_body`](crate::Limits::response_body)
/// is still one.
pub async fn ping(
    config: &Config,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Ping, Error> {
    // One bound for both, as moby's two requests share one context.
    let started = tokio::time::Instant::now();
    let target = Target::of(config);
    let head = Call::new(Method::HEAD, "/_ping", "").unversioned();
    if let Ok(Answer {
        status: StatusCode::OK,
        headers,
        ..
    }) = answered(config, &target, head, timeout, cancel).await
    {
        return Ok(ping_of(&headers));
    }
    let get = Call::new(Method::GET, "/_ping", "").unversioned();
    let left = timeout.saturating_sub(started.elapsed());
    let Answer {
        status,
        headers,
        body,
    } = answered(config, &target, get, left, cancel).await?;
    if status.as_u16() >= 400 {
        let max = config.limits().response_body;
        return Err(Error::from(refusal_of(status, &body, max)));
    }
    Ok(ping_of(&headers))
}

fn ping_of(headers: &http::HeaderMap) -> Ping {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    Ping {
        api_version: header("api-version"),
        os_type: header("ostype"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use crate::test_util::{FakeEngine, Reply};

    const SECS: Duration = Duration::from_secs(5);

    fn config_of(engine: &FakeEngine) -> Config {
        Config::unix(
            engine.socket().to_str().unwrap(),
            crate::config::TEST_LIMITS,
        )
        .api_version("1.44")
        .unwrap()
        .user_agent("raw-get-test/1")
    }

    /// The request carries the version prefix, the query as given and the `User-Agent`, and the
    /// body comes back as the engine sent it, an enum value no model knows included.
    #[tokio::test]
    async fn get_sends_the_path_and_query_and_returns_the_body() {
        let body = r#"[{"Status":{"State":"a-state-of-the-future"}}] trailing"#;
        let engine = FakeEngine::start(&[("/tasks", Reply::ok(body))]).unwrap();
        let got = get(
            &config_of(&engine),
            "/tasks",
            "filters=%7B%7D",
            SECS,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(got, body.as_bytes());
        let requests = engine.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].raw_path(), "/v1.44/tasks?filters=%7B%7D");
        assert_eq!(requests[0].header("user-agent"), Some("raw-get-test/1"));
    }

    /// No query, no `?`.
    #[tokio::test]
    async fn get_without_a_query_sends_no_question_mark() {
        let engine = FakeEngine::start(&[("/info", Reply::ok("{}"))]).unwrap();
        get(
            &config_of(&engine),
            "/info",
            "",
            SECS,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(engine.requests()[0].raw_path(), "/v1.44/info");
    }

    /// A refusal keeps its kind and the engine's message.
    #[tokio::test]
    async fn get_sorts_a_refusal_into_its_kind() {
        let engine = FakeEngine::start(&[(
            "/containers/c1/json",
            Reply::error(404, "No such container: c1"),
        )])
        .unwrap();
        let path = format!("/containers/{}/json", escape_id("c1"));
        let error = get(
            &config_of(&engine),
            &path,
            "",
            SECS,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(
            error.to_string().contains("No such container: c1"),
            "{error}"
        );
    }

    /// The whole call is bounded: a body that stalls after the headers times out.
    #[tokio::test]
    async fn get_bounds_the_body() {
        let engine = FakeEngine::start(&[("/services", Reply::Stalled)]).unwrap();
        let error = get(
            &config_of(&engine),
            "/services",
            "status=true",
            Duration::from_millis(200),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
    }

    /// A cancelled call stops at once.
    #[tokio::test]
    async fn get_stops_when_cancelled() {
        let engine = FakeEngine::start(&[("/services", Reply::Stalled)]).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = get(&config_of(&engine), "/services", "", SECS, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Cancelled);
    }

    fn ping_reply(status: &str, headers: &str, body: &str) -> Reply {
        Reply::Raw(
            format!(
                "HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\n\
                 Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
        )
    }

    /// moby's fast path: a `HEAD` answered `200` is the whole ping, sent without the version
    /// prefix even by a client that has one, and the version is read from the header.
    #[tokio::test]
    async fn ping_reads_the_version_from_a_head() {
        let engine = FakeEngine::start(&[(
            "/_ping",
            ping_reply("200 OK", "Api-Version: 1.47\r\nOstype: linux\r\n", "OK"),
        )])
        .unwrap();
        let got = ping(&config_of(&engine), SECS, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            got,
            Ping {
                api_version: "1.47".to_owned(),
                os_type: "linux".to_owned(),
            }
        );
        let requests = engine.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "HEAD");
        assert_eq!(requests[0].raw_path(), "/_ping");
        assert_eq!(requests[0].header("host"), Some("api.moby.localhost"));
        assert_eq!(requests[0].header("user-agent"), Some("raw-get-test/1"));
    }

    /// A `HEAD` that fails is followed by a `GET`, whose refusal is the ping's error.
    #[tokio::test]
    async fn ping_falls_back_to_a_get_and_reports_its_refusal() {
        let engine = FakeEngine::start(&[(
            "/_ping",
            ping_reply("500 Internal Server Error", "", r#"{"message":"down"}"#),
        )])
        .unwrap();
        let error = ping(&config_of(&engine), SECS, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(error.to_string().contains("down"), "{error}");
        let methods: Vec<String> = engine
            .requests()
            .into_iter()
            .map(|request| request.method)
            .collect();
        assert_eq!(methods, ["HEAD", "GET"]);
    }

    /// A `HEAD` answered other than `200` falls back too, and a `GET` that succeeds without the
    /// header answers an empty version, which moby reads as its newest.
    #[tokio::test]
    async fn ping_without_a_version_header_answers_empty() {
        let engine =
            FakeEngine::start(&[("/_ping", ping_reply("204 No Content", "", ""))]).unwrap();
        let got = ping(&config_of(&engine), SECS, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(got, Ping::default());
        assert_eq!(engine.requests().len(), 2);
    }

    fn limited(engine: &FakeEngine, max: usize) -> Config {
        let limits = crate::Limits {
            response_body: std::num::NonZeroUsize::new(max).unwrap(),
            ..crate::config::TEST_LIMITS
        };
        Config::unix(engine.socket().to_str().unwrap(), limits)
    }

    fn chunked(chunks: &[&str], complete: bool) -> Vec<u8> {
        let mut bytes = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for chunk in chunks {
            bytes.extend_from_slice(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes());
        }
        if complete {
            bytes.extend_from_slice(b"0\r\n\r\n");
        }
        bytes
    }

    async fn get_of(engine: &FakeEngine, max: usize) -> Result<Vec<u8>, Error> {
        get(
            &limited(engine, max),
            "/big",
            "",
            SECS,
            &CancellationToken::new(),
        )
        .await
    }

    fn assert_answer_too_large(error: &Error, max: usize) {
        assert_eq!(error.kind(), ErrorKind::Other, "{error:?}");
        let expected = format!("the engine's answer exceeds {max} bytes");
        assert!(error.to_string().contains(&expected), "{error}");
    }

    #[tokio::test]
    async fn a_declared_body_at_the_limit_passes_and_one_byte_over_is_refused() {
        let engine = FakeEngine::start(&[("/big", Reply::ok("abcd"))]).unwrap();
        assert_eq!(get_of(&engine, 4).await.unwrap(), b"abcd");
        assert_answer_too_large(&get_of(&engine, 3).await.unwrap_err(), 3);
    }

    #[tokio::test]
    async fn a_chunked_body_at_the_limit_passes_and_one_byte_over_is_refused() {
        let reply = Reply::Raw(chunked(&["ab", "cd"], true));
        let engine = FakeEngine::start(&[("/big", reply)]).unwrap();
        assert_eq!(get_of(&engine, 4).await.unwrap(), b"abcd");
        assert_answer_too_large(&get_of(&engine, 3).await.unwrap_err(), 3);
    }

    /// The declared length is enough: no byte of the body is ever sent here.
    #[tokio::test]
    async fn a_declared_length_is_refused_before_any_body_byte() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec();
        let engine = FakeEngine::start(&[("/big", Reply::Held(head))]).unwrap();
        assert_answer_too_large(&get_of(&engine, 4).await.unwrap_err(), 4);
    }

    #[tokio::test]
    async fn a_chunked_body_is_refused_while_the_peer_stays_open() {
        let reply = Reply::Held(chunked(&["abcde"], false));
        let engine = FakeEngine::start(&[("/big", reply)]).unwrap();
        assert_answer_too_large(&get_of(&engine, 4).await.unwrap_err(), 4);
    }

    #[tokio::test]
    async fn an_oversized_refusal_keeps_its_kind() {
        for (status, kind) in [(404, ErrorKind::NotFound), (409, ErrorKind::Conflict)] {
            let reply = Reply::error(status, "a refusal longer than the limit");
            let engine = FakeEngine::start(&[("/big", reply)]).unwrap();
            let error = get_of(&engine, 4).await.unwrap_err();
            assert_eq!(error.kind(), kind, "{error:?}");
            assert!(
                error
                    .to_string()
                    .contains("the engine's refusal exceeds 4 bytes"),
                "{error}"
            );
        }
    }

    #[tokio::test]
    async fn a_connection_cut_mid_body_stays_unavailable() {
        let cut = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec();
        let engine = FakeEngine::start(&[("/big", Reply::Raw(cut))]).unwrap();
        let error = get_of(&engine, 1024).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable, "{error:?}");
    }

    /// Only the headers of a ping are used, so a long success is still one.
    #[tokio::test]
    async fn ping_succeeds_on_an_oversized_get() {
        let engine = FakeEngine::start(&[
            ("HEAD /_ping", Reply::status(500)),
            (
                "GET /_ping",
                ping_reply("200 OK", "Api-Version: 1.47\r\n", "longer than the limit"),
            ),
        ])
        .unwrap();
        let got = ping(&limited(&engine, 4), SECS, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(got.api_version, "1.47");
    }

    #[tokio::test]
    async fn ping_refuses_an_oversized_error() {
        let engine = FakeEngine::start(&[
            ("HEAD /_ping", Reply::status(500)),
            (
                "GET /_ping",
                ping_reply("500 Internal Server Error", "", "longer than the limit"),
            ),
        ])
        .unwrap();
        let error = ping(&limited(&engine, 4), SECS, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other, "{error:?}");
        assert!(
            error
                .to_string()
                .contains("the engine's refusal exceeds 4 bytes"),
            "{error}"
        );
    }

    /// A ping that cannot reach the engine fails with `Unavailable`.
    #[tokio::test]
    async fn ping_of_a_missing_socket_is_unavailable() {
        let config = Config::unix("/nonexistent/docker.sock", crate::config::TEST_LIMITS);
        let error = ping(&config, SECS, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unavailable);
    }
}
