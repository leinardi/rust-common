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
//!
//! [`attach`] sends moby client v0.6.2's `ContainerAttach` with `Stream`, `Stdout` and `Stderr` set:
//! `POST /containers/<id>/attach?stderr=1&stdout=1&stream=1` with `Content-Type: text/plain`, no
//! body, and the connection taken over (`Connection: Upgrade`, `Upgrade: tcp`, moby's
//! `postHijacked`). The query is Go's `url.Values.Encode`, which sorts the keys. The ID is trimmed
//! and escaped as moby's `trimID` and `url.URL` do. The output is read with the framing the caller
//! passes, never guessed from the bytes as bollard's `attach_container` does: a TTY session or a
//! log line starting with `\x01` defeats that guess.
//!
//! As for exec's start, a dial that fails is worded as moby's `doRequest` words it; moby's hijack
//! has its own text there (`cannot connect to the Docker daemon. Is 'docker daemon' running on this
//! host?: <err>`), which is not reproduced.

use std::time::Duration;

use bollard::errors::Error as BollardError;
use bollard::models::ContainerWaitResponse;
use bytes::Bytes;
use http::Method;
use http_body_util::{BodyExt as _, Collected};
use hyper_util::rt::TokioIo;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::demux::{DemuxError, Demuxer, Framing, Pipe};
use crate::error::Error;
use crate::pump::pump_stream;
use crate::stream::{Call, Target, escape_id, establish, failure, trim_id, upgrade};

/// The query of moby's `ContainerAttach` with `Stream`, `Stdout` and `Stderr`, its keys sorted as
/// `url.Values.Encode` sorts them.
const ATTACH_QUERY: &str = "stderr=1&stdout=1&stream=1";

/// A piece of a container's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Written to the container's stdout (all of a raw, TTY stream).
    Stdout(Vec<u8>),
    /// Written to the container's stderr.
    Stderr(Vec<u8>),
}

/// Attaches to the container `id`'s stdout and stderr and reads them as `framing` says:
/// [`Framing::Multiplexed`] for a container created without a TTY, [`Framing::Raw`] for one with a
/// TTY (all of it is then [`Output::Stdout`], each piece as it arrives). The caller knows which
/// from the `Tty` it created the container with, as moby's client does.
///
/// Only establishing the stream is bounded, by `timeout`, and `cancel` interrupts it; the stream
/// lasts as long as the container's output, and the pump ends as [`pump_stream`] says (`capacity`
/// is its buffer, at least one). A stream the engine breaks off, or that names an error or an
/// unknown stream, yields that error and ends. Must be called inside a tokio runtime.
///
/// `config` is the client's: the one it was connected from, or the one
/// [`negotiate`](crate::negotiate) returned with it. The request carries its `User-Agent` and API
/// version, as the client's own.
///
/// # Errors
///
/// An empty `id` (whitespace trimmed, as moby's client does) is refused before sending, as
/// `Other`; an answer other than `101` carries its status and moby's text, `unable to upgrade to
/// tcp, received <status>`: 404 (no such container) is `NotFound`, 409 `Conflict`; the bound's
/// `Timeout`, `Cancelled`, and `Unavailable` when the engine cannot be reached.
pub async fn attach(
    config: &Config,
    id: &str,
    framing: Framing,
    timeout: Duration,
    capacity: usize,
    cancel: CancellationToken,
) -> Result<mpsc::Receiver<Result<Output, Error>>, Error> {
    let id = trim_id("container", id)?;
    let path = format!("/containers/{}/attach", escape_id(id));
    let call = Call::new(Method::POST, &path, ATTACH_QUERY).plain_text();
    let upgraded = upgrade(config, &Target::of(config), call, timeout, &cancel).await?;
    let state = Some((TokioIo::new(upgraded), Demuxer::new(framing)));
    let output = futures_util::stream::unfold(state, |state| async move {
        let (mut io, mut demuxer) = state?;
        match demuxer.read_frame(&mut io).await {
            Ok(Some(frame)) => {
                let bytes = frame.bytes.to_vec();
                let output = match frame.pipe {
                    Pipe::Stdout => Output::Stdout(bytes),
                    Pipe::Stderr => Output::Stderr(bytes),
                };
                Some((Ok(output), Some((io, demuxer))))
            }
            Ok(None) => None,
            // The stream ends with its error: a failed read is not retried.
            Err(error) => Some((Err(stream_error(error)), None)),
        }
    });
    Ok(pump_stream(output, capacity, cancel))
}

/// A stream that cannot be read on, in bollard's error: a broken connection as its I/O error, the
/// engine's error or an unknown stream as their text.
fn stream_error(error: DemuxError) -> BollardError {
    match error {
        DemuxError::Io(error) => BollardError::from(error),
        DemuxError::Daemon(_) | DemuxError::Unrecognized(_) => failure(error.to_string()),
    }
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
    let path = format!("/containers/{}/wait", escape_id(id));
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
