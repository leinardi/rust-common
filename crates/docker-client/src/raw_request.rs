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

//! Changes sent as moby's Go client sends them: `POST` and `DELETE` with the path and the query the
//! caller builds, under the client's version prefix.
//!
//! bollard's own mutations differ from moby's on the wire: its URL join drops the `/v1.xx` prefix
//! (`Url::join` with an absolute path), so they reach the engine's newest API instead of the
//! negotiated one; its option structs send every flag (`?v=false&force=true&link=false` where moby
//! sends `?force=1`); and its prune filters are lists where moby writes maps. These send what the
//! caller wrote: an ID goes into the path through
//! [`escape_id`](crate::raw_get::escape_id), and the query is sent as given.
//!
//! As moby's `checkResponseErr`, any answer from `200` to `399` is a success (a container already
//! stopped answers `304`), and the rest a refusal with its kind and the engine's message. Neither
//! call sends a body: moby's `post` sends none for a `nil` body, nor a `Content-Type`.
//!
//! The wire is moby/moby `client/request.go`'s at client/v0.6.2 (`post`, `delete`, `sendRequest`,
//! `checkResponseErr`); the code is written anew, not extracted.

use std::time::Duration;

use http::Method;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::stream::{Call, Target, answered, refusal};

/// `POST <path>?<query>` (no `?` for an empty `query`) with no body, and the whole body of the
/// engine's answer. `path` is the path without the version prefix (`/containers/<id>/restart`),
/// and `query` the encoded query string without its `?`. `timeout` bounds the whole call, the
/// body included; `cancel` ends it at once.
///
/// # Errors
///
/// For an answer of `400` or more, the engine's refusal sorted into its kind (404 is `NotFound`,
/// 409 `Conflict`, the rest `Other`) with the engine's message; the bound's `Timeout`,
/// `Cancelled`, and `Unavailable` when the engine cannot be reached or the connection breaks.
pub async fn post(
    config: &Config,
    path: &str,
    query: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, Error> {
    send(
        config,
        Call::new(Method::POST, path, query),
        timeout,
        cancel,
    )
    .await
}

/// `DELETE <path>?<query>`, as [`post`].
///
/// # Errors
///
/// As [`post`].
pub async fn delete(
    config: &Config,
    path: &str,
    query: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, Error> {
    send(
        config,
        Call::new(Method::DELETE, path, query),
        timeout,
        cancel,
    )
    .await
}

async fn send(
    config: &Config,
    call: Call<'_>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, Error> {
    let (status, _, body) = answered(config, &Target::of(config), call, timeout, cancel).await?;
    if (200..400).contains(&status.as_u16()) {
        return Ok(body.to_vec());
    }
    Err(Error::from(refusal(status, &body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use crate::test_util::{FakeEngine, Reply};

    const SECS: Duration = Duration::from_secs(5);

    fn config_of(engine: &FakeEngine) -> Config {
        Config::unix(engine.socket().to_str().unwrap())
            .api_version("1.44")
            .unwrap()
            .user_agent("raw-request-test/1")
    }

    /// The request carries the method, the version prefix, the query as given and the
    /// `User-Agent`, no body and no `Content-Type`, and the answer's body comes back.
    #[tokio::test]
    async fn post_sends_the_path_and_query_and_returns_the_body() {
        let body = r#"{"ContainersDeleted":null,"SpaceReclaimed":0}"#;
        let engine = FakeEngine::start(&[("/containers/prune", Reply::ok(body))]).unwrap();
        let got = post(
            &config_of(&engine),
            "/containers/prune",
            "filters=%7B%7D",
            SECS,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(got, body.as_bytes());
        let requests = engine.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].raw_path(),
            "/v1.44/containers/prune?filters=%7B%7D"
        );
        assert_eq!(requests[0].header("user-agent"), Some("raw-request-test/1"));
        assert_eq!(requests[0].header("content-type"), None);
        assert!(requests[0].body.is_empty());
    }

    /// `DELETE`, and no query, no `?`.
    #[tokio::test]
    async fn delete_without_a_query_sends_no_question_mark() {
        let engine = FakeEngine::start(&[("/secrets/s1", Reply::status(204))]).unwrap();
        let got = delete(
            &config_of(&engine),
            "/secrets/s1",
            "",
            SECS,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(got.is_empty());
        let requests = engine.requests();
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(requests[0].raw_path(), "/v1.44/secrets/s1");
    }

    /// moby's success range: a `304` (a container already stopped) is not a refusal.
    #[tokio::test]
    async fn an_answer_below_400_is_a_success() {
        let engine = FakeEngine::start(&[("/containers/c1/stop", Reply::status(304))]).unwrap();
        post(
            &config_of(&engine),
            "/containers/c1/stop",
            "",
            SECS,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    }

    /// A refusal keeps its kind and the engine's message.
    #[tokio::test]
    async fn a_refusal_is_sorted_into_its_kind() {
        let engine = FakeEngine::start(&[
            ("/containers/c1", Reply::error(404, "No such container: c1")),
            ("/networks/n1", Reply::error(409, "network n1 is in use")),
            ("/volumes/v1", Reply::error(500, "boom")),
        ])
        .unwrap();
        let config = config_of(&engine);
        let cancel = CancellationToken::new();
        for (path, kind, message) in [
            (
                "/containers/c1",
                ErrorKind::NotFound,
                "No such container: c1",
            ),
            ("/networks/n1", ErrorKind::Conflict, "network n1 is in use"),
            ("/volumes/v1", ErrorKind::Other, "boom"),
        ] {
            let error = delete(&config, path, "", SECS, &cancel).await.unwrap_err();
            assert_eq!(error.kind(), kind, "{path}");
            assert!(error.to_string().contains(message), "{path}: {error}");
        }
    }

    /// The whole call is bounded: a body that stalls after the headers times out.
    #[tokio::test]
    async fn post_bounds_the_body() {
        let engine = FakeEngine::start(&[("/images/prune", Reply::Stalled)]).unwrap();
        let error = post(
            &config_of(&engine),
            "/images/prune",
            "",
            Duration::from_millis(200),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
    }

    /// A cancelled call stops at once.
    #[tokio::test]
    async fn delete_stops_when_cancelled() {
        let engine = FakeEngine::start(&[("/volumes/v1", Reply::Stalled)]).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = delete(&config_of(&engine), "/volumes/v1", "", SECS, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Cancelled);
    }
}
