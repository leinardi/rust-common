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

//! A bollard stream as a channel.
//!
//! Extracted from swarm-rootless-docker/src/docker/mod.rs at e0961f77655f (the attach pump's
//! spawned task, generalized and given a cancellation).

use std::pin::pin;

use bollard::errors::Error as BollardError;
use futures_util::{Stream, StreamExt as _};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::Error;

/// Spawns a task that moves `stream`'s items into a channel of `capacity` (at least one) and
/// returns its receiver; each error is sorted into an [`Error`] and the stream goes on, as it does
/// for bollard.
///
/// The task ends, dropping `stream` (which closes its connection), when the stream ends, when the
/// receiver is dropped, even while the stream is idle, or when `cancel` fires, even while the
/// receiver is full and not read. The receiver then yields what was already sent and closes. Must
/// be called inside a tokio runtime.
#[must_use]
pub fn pump_stream<S, T>(
    stream: S,
    capacity: usize,
    cancel: CancellationToken,
) -> mpsc::Receiver<Result<T, Error>>
where
    S: Stream<Item = Result<T, BollardError>> + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = mpsc::channel(capacity.max(1));
    tokio::spawn(async move {
        let mut stream = pin!(stream);
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = sender.closed() => break,
                next = stream.next() => next,
            };
            let Some(item) = next else { break };
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                sent = sender.send(item.map_err(Error::from)) => {
                    if sent.is_err() {
                        break;
                    }
                }
            }
        }
    });
    receiver
}
