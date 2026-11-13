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
//! model, and renames one (`Healthcheck`), so the spec is kept as a [`serde_json::Value`]: the
//! caller changes what it means to (`TaskTemplate.ForceUpdate`, the image) and sends the rest as it
//! came. Integers keep their exact value up to `u64` and `i64`; a number beyond both would become
//! a float, which no service spec holds. The body is written with the keys sorted and without
//! Go's `\u0026`-style escapes, which the engine's decoder reads the same. As Go's decoder, the
//! first JSON value of an answer is read and anything after it ignored.
//!
//! The requests are moby's Go client's: `ServiceInspect` without `InsertDefaults` sends no query,
//! and `ServiceUpdate` sends `?version=<index>` with the spec as its JSON body and no registry
//! authentication header (runhold sends none). moby's `ServiceUpdate` also checks the spec
//! (a container spec and a plugin spec together, a runtime that does not match) and adds `:latest`
//! to an image with neither a tag nor a digest before sending; a raw update does neither, so those
//! are the caller's.
//!
//! The wire is moby/moby `client/service_inspect.go` and `client/service_update.go`'s at
//! client/v0.6.2; the code is written anew, not extracted.

use std::time::Duration;

use bollard::errors::Error as BollardError;
use http::Method;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::stream::{Call, Target, complete, escape_id, failure, trim_id};

/// The first JSON value of `body`, anything after it ignored, as Go's `json.Decoder` reads an
/// answer.
fn first_value(body: &[u8]) -> Result<Value, Error> {
    match serde_json::Deserializer::from_slice(body)
        .into_iter::<Value>()
        .next()
    {
        Some(decoded) => decoded.map_err(|error| Error::from(BollardError::from(error))),
        None => Err(Error::from(failure("the answer is empty".to_owned()))),
    }
}

/// A service as the engine answered it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RawService {
    /// The whole inspect answer (`ID`, `Version`, `Spec`, `Endpoint`, ...).
    pub value: Value,
    /// `Version.Index`, which an update must name.
    pub version: u64,
}

impl RawService {
    /// The answer's `Spec`, the body an update sends back.
    #[must_use]
    pub fn spec(&self) -> Option<&Value> {
        self.value.get("Spec")
    }
}

/// `GET /services/{id}`, the answer kept whole. `timeout` bounds the whole call; `cancel` ends it
/// at once.
///
/// # Errors
///
/// An empty `id` (whitespace trimmed, as moby's client does) is refused before sending, as
/// `Other`; the engine's refusal sorted into its kind (404 for a service that is not there); an
/// answer that is not JSON, or whose `Version.Index` is missing or not an unsigned integer
/// (`Other`); the bound's `Timeout`, `Cancelled`, and `Unavailable` when the engine cannot be
/// reached.
pub async fn inspect_service_raw(
    config: &Config,
    id: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<RawService, Error> {
    let id = trim_id("service", id)?;
    let path = format!("/services/{}", escape_id(id));
    let body = complete(
        config,
        &Target::of(config),
        Call::new(Method::GET, &path, ""),
        timeout,
        cancel,
    )
    .await?;
    let value = first_value(&body)?;
    let version = value
        .get("Version")
        .and_then(|version| version.get("Index"))
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            Error::from(failure(
                "the service inspect answer has no unsigned Version.Index".to_owned(),
            ))
        })?;
    Ok(RawService { value, version })
}

/// `POST /services/{id}/update?version=<version>` with `spec` as the body, and the warnings the
/// engine answered with (none when it sent `null`). `timeout` bounds the whole call; `cancel` ends
/// it at once.
///
/// # Errors
///
/// An empty `id` is refused before sending, as `Other`; the engine's refusal sorted into its kind:
/// 404 is a service that is not there, a 409 [`ErrorKind::Conflict`](crate::ErrorKind::Conflict).
/// A stale `version` (another update came first) is answered by Docker 29 with a 500, `rpc error:
/// ... update out of sequence`, so it is `Other`, as moby's Go client sees it too; an answer that
/// does not decode (`Other`); the bound's `Timeout`, `Cancelled`, and `Unavailable` when the
/// engine cannot be reached.
pub async fn update_service_raw(
    config: &Config,
    id: &str,
    version: u64,
    spec: &Value,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Vec<String>, Error> {
    let id = trim_id("service", id)?;
    let path = format!("/services/{}/update", escape_id(id));
    let query = format!("version={version}");
    let call = Call::new(Method::POST, &path, &query).json(spec.to_string());
    let body = complete(config, &Target::of(config), call, timeout, cancel).await?;
    let answer = first_value(&body)?;
    match answer.get("Warnings") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>()
            .ok_or_else(|| Error::from(failure("a service update warning is not text".to_owned()))),
        Some(_) => Err(Error::from(failure(
            "the service update Warnings are not a list".to_owned(),
        ))),
    }
}
