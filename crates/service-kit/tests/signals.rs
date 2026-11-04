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

//! The signal helpers against real signals, sent to this test process with `kill(1)`. A handler
//! stays installed for the life of a process, so whether a function installs its handlers when
//! called or when polled is checked in a child: this test binary run again on [`probe`] alone,
//! which signals itself and either survives or is killed.
//!
//! Each test installs the handlers it relies on before sending the signal, so the tests do not
//! depend on one another's order.
//!
//! Written for this crate; swarm-device-access's binary tests (tests/shutdown.rs at
//! 8b4ab3a3e417) cover the same behaviour through its daemon.

#![cfg(unix)]

use std::os::unix::process::ExitStatusExt as _;
use std::process::Command;
use std::time::Duration;

use service_kit::signal::{hangup_channel, lazy_shutdown_signal, shutdown_signal};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::timeout;

/// Selects the probe's mode in the child; unset in a normal run, where [`probe`] does nothing.
const PROBE: &str = "SERVICE_KIT_SIGNAL_PROBE";

/// How long a test waits for a signal to arrive.
const WAIT: Duration = Duration::from_secs(10);

fn kill(signal: &str) {
    let status = Command::new("kill")
        .args([signal, &std::process::id().to_string()])
        .status()
        .unwrap_or_else(|error| panic!("kill {signal}: {error}"));
    assert!(status.success(), "kill {signal}: {status}");
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| panic!("runtime: {error}"))
}

/// Runs in the child: makes the shutdown future in the mode `PROBE` names, sends itself `SIGTERM`
/// without polling the future, and returns if it is still alive after a while.
#[test]
fn probe() {
    let Ok(mode) = std::env::var(PROBE) else {
        return;
    };
    runtime().block_on(async {
        let _unpolled: std::pin::Pin<Box<dyn Future<Output = ()> + Send>> = match mode.as_str() {
            "eager" => Box::pin(shutdown_signal()),
            "lazy" => Box::pin(lazy_shutdown_signal()),
            other => panic!("{PROBE}={other}"),
        };
        kill("-TERM");
        tokio::time::sleep(Duration::from_millis(500)).await;
    });
}

/// Runs [`probe`] in a child in `mode`, and returns the signal that killed it, if one did.
fn run_probe(mode: &str) -> Option<i32> {
    let exe = std::env::current_exe().unwrap_or_else(|error| panic!("current_exe: {error}"));
    let output = Command::new(exe)
        .args(["--exact", "probe", "--test-threads=1"])
        .env(PROBE, mode)
        .output()
        .unwrap_or_else(|error| panic!("probe {mode}: {error}"));
    let status = output.status;
    if status.signal().is_none() {
        assert!(
            status.success(),
            "probe {mode}: {status}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    status.signal()
}

#[test]
fn shutdown_signal_catches_a_signal_sent_before_it_is_polled() {
    assert_eq!(run_probe("eager"), None);
}

#[test]
fn lazy_shutdown_signal_leaves_the_default_action_until_polled() {
    assert_eq!(run_probe("lazy"), Some(15));
}

#[test]
fn shutdown_signal_resolves_on_sigint_and_on_sigterm() {
    runtime().block_on(async {
        let interrupt = shutdown_signal();
        kill("-INT");
        timeout(WAIT, interrupt).await.unwrap();

        let terminate = shutdown_signal();
        kill("-TERM");
        timeout(WAIT, terminate).await.unwrap();
    });
}

#[test]
fn lazy_shutdown_signal_resolves_once_polled() {
    runtime().block_on(async {
        let task = tokio::spawn(lazy_shutdown_signal());
        // Let the task run up to its first wait, which installs the handlers.
        tokio::time::sleep(Duration::from_millis(100)).await;
        kill("-TERM");
        timeout(WAIT, task).await.unwrap().unwrap();
    });
}

#[test]
fn hangup_channel_holds_one_pending_signal() {
    runtime().block_on(async {
        let mut hangups = hangup_channel();
        assert_eq!(hangups.max_capacity(), 1);
        kill("-HUP");
        timeout(WAIT, hangups.recv()).await.unwrap().unwrap();

        kill("-HUP");
        tokio::time::sleep(Duration::from_millis(200)).await;
        kill("-HUP");
        tokio::time::sleep(Duration::from_millis(200)).await;
        timeout(WAIT, hangups.recv()).await.unwrap().unwrap();
        assert_eq!(hangups.try_recv(), Err(TryRecvError::Empty));
    });
}

/// A runtime without the I/O driver cannot install a handler: tokio panics rather than returning
/// an error, so the functions' error branch is not this case.
#[test]
#[should_panic(expected = "IO is disabled")]
fn without_the_io_driver_tokio_panics() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let _hangups = hangup_channel();
    });
}
