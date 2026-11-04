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

//! The shutdown and reload signals, Go's `signal.NotifyContext(ctx, SIGINT, SIGTERM)` and
//! `signal.Notify(ch, SIGHUP)`.
//!
//! When a handler is installed matters: before it, the signal's default action terminates the
//! process; after it, the signal is caught, and tokio keeps the handler for the life of the
//! process, so later signals are swallowed. [`shutdown_signal`] installs its handlers when it is
//! called, [`lazy_shutdown_signal`] when it is first polled; a program picks the one that matches
//! where Go called `signal.NotifyContext`. Installing a handler needs the context of a tokio
//! runtime with the I/O driver (`enable_io`, or [`build_runtime`](crate::runtime::build_runtime));
//! tokio panics without one.
//!
//! Extracted from swarm-device-access/src/run.rs at 8b4ab3a3e417 (`shutdown_signal`,
//! `hangup_channel`), with `lazy_shutdown_signal` from swarm-scheduler-exporter/src/run.rs at
//! c9e79afa8810 (its `shutdown_signal`).

use std::future::Future;

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

/// Installs the `SIGINT` and `SIGTERM` handlers now, and returns what resolves on the first of
/// them, or on one that arrived between this call and the first poll. When a handler cannot be
/// installed (the operating system refuses it, or the runtime is shutting down) the future never
/// resolves.
///
/// # Panics
///
/// Called outside the context of a tokio runtime with the I/O driver.
pub fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    let registered = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    );
    async move {
        let (Ok(mut interrupt), Ok(mut terminate)) = registered else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }
}

/// [`shutdown_signal`], with the handlers installed when the future is first polled rather than
/// when it is made: until then `SIGINT` and `SIGTERM` keep their default action and terminate the
/// process. A program that makes the future early (in `main`) but means to catch the signals only
/// once its startup is done (Go's `signal.NotifyContext` after the flags and a probe) uses this.
///
/// # Panics
///
/// Polled outside the context of a tokio runtime with the I/O driver.
pub async fn lazy_shutdown_signal() {
    shutdown_signal().await;
}

/// Installs the `SIGHUP` handler now, and forwards each signal to a channel that holds one
/// pending signal, as Go's buffered channel of one does: a `SIGHUP` while one is pending is
/// dropped. The handler stays for the life of the process, so a `SIGHUP` after the receiver is
/// dropped is swallowed. When the handler cannot be installed (the operating system refuses it,
/// or the runtime is shutting down) the channel is closed at once.
///
/// # Panics
///
/// Called outside the context of a tokio runtime with the I/O driver.
#[must_use]
pub fn hangup_channel() -> mpsc::Receiver<()> {
    let (sender, receiver) = mpsc::channel(1);
    if let Ok(mut hangup) = signal(SignalKind::hangup()) {
        tokio::spawn(async move {
            while hangup.recv().await.is_some() {
                let _ = sender.try_send(());
            }
        });
    }
    receiver
}
