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

//! A service read and written back as the JSON the engine holds, so a field no typed model knows
//! survives the round trip.
//!
//! runhold's Go client restarts a service and re-pins its image digest by inspecting it and
//! sending its spec back. bollard's typed `ServiceSpec` would drop every field newer than its

//! Reads answered as the engine's bytes, for a caller that decodes them itself: bollard's models
//! close every enum (a task state, a container state), so one value the model does not know fails
//! the whole answer, where moby's Go client decodes those fields as plain strings and takes any.
//!
//! A read goes out as bollard's own calls do, over the same endpoint the [`Config`] describes,
//! with the same `User-Agent` and the API version prefix the client sends ([`negotiate`]'s, once
//! it ran). The caller builds the path and the query: an ID goes into the path through
//! [`escape_id`], as moby's client escapes it, and the query is sent as given.
//!
//! [`negotiate`]: crate::negotiate

use std::time::Duration;

use http::Method;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::stream::{Call, Target, complete};

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
/// the engine cannot be reached or the connection breaks.
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
    Ok(body.to_vec())
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
}
