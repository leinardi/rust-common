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

//! The shared shutdown state of a server: what asks it to stop, what force-closes it, and the
//! tracker that owns every connection and every task a handler hands over.
//!
//! Extracted from bitgauge/src/server/conn.rs at d841694700a5 (`Lifecycle`), with `spawn` added
//! for tasks that outlive their request, such as a WebSocket on an upgraded connection.

use std::future::Future;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The shutdown signals and the tracker of one server.
///
/// Clone it freely: the clones share their state. [`serve`](super::serve) puts a clone in the
/// extensions of every request, so a handler that upgrades the connection can
/// [`spawn`](Lifecycle::spawn) the upgraded task onto the tracker and watch
/// [`cut_token`](Lifecycle::cut_token).
///
/// The sequence, driven by `serve`: [`stop`](Lifecycle::stop) (no new connections, each
/// connection shuts down gracefully), then the drain deadline, then the cut, which every
/// connection and every task that watches it must obey by dropping its socket.
#[derive(Debug, Clone, Default)]
pub struct Lifecycle {
    pub(super) stop: CancellationToken,
    pub(super) cut: CancellationToken,
    pub(super) tracker: TaskTracker,
}

impl Lifecycle {
    /// A server that has not been asked to stop.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the server to stop: no new connection, graceful shutdown of the running ones.
    pub fn stop(&self) {
        self.stop.cancel();
    }

    /// The token [`stop`](Lifecycle::stop) cancels.
    #[must_use]
    pub fn stop_token(&self) -> &CancellationToken {
        &self.stop
    }

    /// The token `serve` cancels when the drain deadline passes: whoever holds a connection or a
    /// socket must drop it.
    #[must_use]
    pub fn cut_token(&self) -> &CancellationToken {
        &self.cut
    }

    /// The tracker `serve` waits on while it drains.
    #[must_use]
    pub fn tracker(&self) -> &TaskTracker {
        &self.tracker
    }

    /// Spawns `task` on the tracker, so the drain waits for it. A task that holds a connection
    /// beyond its request (an upgraded one) must select on [`cut_token`](Lifecycle::cut_token)
    /// and return when it fires: nothing aborts it.
    pub fn spawn<F>(&self, task: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tracker.spawn(task)
    }
}
