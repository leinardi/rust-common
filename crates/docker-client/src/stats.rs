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

//! One sample of a container's resource usage, with the previous one inside it.
//!
//! The request is moby's Go client's `ContainerStats` with `Stream: false` and
//! `IncludePreviousSample: true`: `GET /containers/{id}/stats?stream=false`, no `one-shot`. The
//! engine then collects two cycles and answers the second, so `precpu_stats` is a real previous
//! sample and a CPU delta is an instantaneous reading; a one-shot answer leaves it zeroed. There is
//! no streaming variant: runhold's Go client never streams stats.
//!
//! The answer is decoded into bollard's `ContainerStatsResponse`, which carries every field
//! runhold's Go formulas read (`cpu_stats` and `precpu_stats` with `cpu_usage.total_usage`,
//! `percpu_usage`, `system_cpu_usage` and `online_cpus`; `memory_stats` with `usage`, `limit` and
//! the `stats` map holding `inactive_file` or `total_inactive_file`). As Go's `json.Decoder`, the
//! first JSON value is read and anything after it is ignored.
//!
//! The wire is moby/moby `client/container_stats.go`'s at client/v0.6.2; the code is written anew,
//! not extracted.

use std::time::Duration;

use bollard::errors::Error as BollardError;
use bollard::models::ContainerStatsResponse;
use http::Method;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::stream::{Call, Target, complete, failure, trim_id};

/// One sample of the container `id`'s usage, with the previous one. `timeout` bounds the whole call
/// (the engine takes about a second to collect the two cycles); `cancel` ends it at once.
///
/// # Errors
///
/// An empty `id` (whitespace trimmed, as moby's client does) is refused before sending, as
/// `Other`; the engine's refusal sorted into its kind (404 for a container that is not there); an
/// answer that does not decode (`Other`); the bound's `Timeout`, `Cancelled`, and `Unavailable`
/// when the engine cannot be reached.
pub async fn stats_once(
    config: &Config,
    id: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<ContainerStatsResponse, Error> {
    let id = trim_id("container", id)?;
    let path = format!("/containers/{id}/stats");
    let body = complete(
        config,
        &Target::of(config),
        Call::new(Method::GET, &path, "stream=false"),
        timeout,
        cancel,
    )
    .await?;
    match serde_json::Deserializer::from_slice(&body)
        .into_iter::<ContainerStatsResponse>()
        .next()
    {
        Some(decoded) => decoded.map_err(|error| Error::from(BollardError::from(error))),
        None => Err(Error::from(failure("the stats answer is empty".to_owned()))),
    }
}
