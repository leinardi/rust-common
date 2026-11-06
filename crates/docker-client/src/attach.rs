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
use bollard::query_parameters::{AttachContainerOptionsBuilder, WaitContainerOptionsBuilder};
use futures_util::StreamExt as _;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, bounded};
use crate::pump::pump_stream;

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

/// Asks for the container `id`'s exit code under `condition`. Nothing is awaited here: a spawned
/// task sends the request at once, and ends when the receiver is dropped or `cancel` fires (the
/// receiver then reports a closed channel). Must be called inside a tokio runtime.
///
/// **Registration is not observable through bollard**, which hides the response headers, so this
/// cannot say when the engine has the waiter, and a `start` made right after the call may reach
/// the engine first. A wait that arrives after an auto-remove container is gone is answered with a
/// 404, which the receiver delivers as an [`Error`] of kind `NotFound`: a caller waiting for
/// [`WaitCondition::Removed`] must read that as "already removed, exit code unknown", not as a
/// failure.
///
/// bollard reports a non-zero exit as an error carrying the code; it is the exit code here, not a
/// failure.
#[must_use]
pub fn wait(
    docker: &Docker,
    id: &str,
    condition: WaitCondition,
    cancel: CancellationToken,
) -> oneshot::Receiver<Result<i64, Error>> {
    let options = WaitContainerOptionsBuilder::new()
        .condition(condition.as_str())
        .build();
    let mut stream = Box::pin(docker.wait_container(id, Some(options)));
    let (mut sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = sender.closed() => return,
            next = stream.next() => exit_code(next),
        };
        let _ = sender.send(result);
    });
    receiver
}

fn exit_code(next: Option<Result<ContainerWaitResponse, BollardError>>) -> Result<i64, Error> {
    match next {
        Some(Ok(response)) => Ok(response.status_code),
        Some(Err(BollardError::DockerContainerWaitError { code, .. })) => Ok(code),
        Some(Err(error)) => Err(Error::from(error)),
        None => Err(Error::from(BollardError::DockerStreamError {
            error: "the wait stream ended empty".to_owned(),
        })),
    }
}
