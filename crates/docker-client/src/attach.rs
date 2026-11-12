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

//! A container's output, and its exit code.
//!
//! Extracted from swarm-rootless-docker/src/docker/mod.rs at e0961f77655f (`Bollard::attach` and
//! `Bollard::wait`).

use std::time::Duration;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::models::ContainerWaitResponse;
use bollard::query_parameters::AttachContainerOptionsBuilder;
use bytes::Bytes;
use futures_util::StreamExt as _;
use http::Method;
use http_body_util::{BodyExt as _, Collected};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::{Error, bounded};
use crate::pump::pump_stream;
use crate::stream::{Call, Target, establish};

/// A piece of a container's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Written to the container's stdout.
    Stdout(Vec<u8>),
    /// Written to the container's stderr.
    Stderr(Vec<u8>),
}

/// Attaches to the container `id`'s stdout and stderr. Only establishing the stream is bounded, by
/// `timeout`; the stream lasts as long as the container's output, and the pump ends as
/// [`pump_stream`] says (`capacity` is its buffer).
///
/// # Errors
///
/// The attach call's failure, sorted into its kind, [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) included.
pub async fn attach(
    docker: &Docker,
    id: &str,
    timeout: Duration,
    capacity: usize,
    cancel: CancellationToken,
) -> Result<mpsc::Receiver<Result<Output, Error>>, Error> {
    let options = AttachContainerOptionsBuilder::new()
        .stream(true)
        .stdout(true)
        .stderr(true)
        .build();
    let attached = bounded(timeout, docker.attach_container(id, Some(options)))
        .await?
        .output;
    let output = attached.map(|item| {
        item.map(|output| match output {
            LogOutput::StdErr { message } => Output::Stderr(message.to_vec()),
            LogOutput::StdOut { message }
            | LogOutput::Console { message }
            | LogOutput::StdIn { message } => Output::Stdout(message.to_vec()),
        })
    });
    Ok(pump_stream(output, capacity, cancel))
}

/// When the engine answers a wait: the `condition` query of `POST /containers/{id}/wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitCondition {
    /// The container is not running: an already stopped one answers at once, so the wait may be
    /// asked for late.
    NotRunning,
    /// The next exit, which a container that is not running yet will make once started.
    NextExit,
    /// The container is removed (an auto-remove one, once it exited); the answer is its exit code.
    Removed,
}

impl WaitCondition {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotRunning => "not-running",
            Self::NextExit => "next-exit",
            Self::Removed => "removed",
        }
    }
}

/// Asks for the container `id`'s exit code under `condition`, and returns once the engine has
/// answered the response headers: that is when it holds the waiter, so a `start` made after this
/// returns cannot overtake the wait, and an auto-removed container that exits at once still
/// delivers its code (moby's Go client returned from `ContainerWait` at the same point). `timeout`
/// bounds the connection and the headers, not the wait itself; `cancel` interrupts that
/// establishment too. A spawned task then reads the exit
/// code, and ends when the receiver is dropped or `cancel` fires (the receiver then reports a
/// closed channel). Must be called inside a tokio runtime.
///
/// `config` is the client's: the one it was connected from, or the one
/// [`negotiate`](crate::negotiate) returned with it. The request carries its `User-Agent` and API
/// version, as the client's own.
///
/// A non-zero exit is the exit code here, not a failure. The code is the answer's `StatusCode`.
///
/// # Errors
///
/// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when the headers do not come in `timeout`,
/// [`ErrorKind::Cancelled`](crate::ErrorKind::Cancelled) when `cancel` fires first, the engine's
/// refusal sorted into its kind (404 for a container that is not there), `Unavailable`
/// when it cannot be reached.
pub async fn wait(
    config: &Config,
    id: &str,
    condition: WaitCondition,
    timeout: Duration,
    cancel: CancellationToken,
) -> Result<oneshot::Receiver<Result<i64, Error>>, Error> {
    let (mut sender, receiver) = oneshot::channel();
    let target = Target::of(config);
    let path = format!("/containers/{id}/wait");
    let query = format!("condition={}", condition.as_str());
    let body = establish(
        config,
        &target,
        Call::new(Method::POST, &path, &query),
        timeout,
        &cancel,
    )
    .await?;
    tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = sender.closed() => return,
            collected = body.collect() => decode_exit(collected),
        };
        let _ = sender.send(result);
    });
    Ok(receiver)
}

fn decode_exit(collected: Result<Collected<Bytes>, hyper::Error>) -> Result<i64, Error> {
    let bytes = collected
        .map_err(|error| Error::from(BollardError::from(error)))?
        .to_bytes();
    serde_json::from_slice::<ContainerWaitResponse>(&bytes)
        .map(|response| response.status_code)
        .map_err(|error| Error::from(BollardError::from(error)))
}
