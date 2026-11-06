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

//! The runtime a service's `main` builds by hand, rather than with `#[tokio::main]`, so that its
//! shutdown is bounded: dropping a runtime waits for its blocking tasks with no bound at all, while
//! [`run_until_exit`] gives them only what is left of the program's shutdown deadline.
//!
//! A `main` keeps its own message and exit code for a runtime that cannot be built:
//!
//! ```no_run
//! use std::io::Write as _;
//! use std::process::ExitCode;
//!
//! use service_kit::runtime::{Exit, build_runtime, run_until_exit};
//!
//! async fn run() -> Exit<i32> {
//!     Exit::early(0)
//! }
//!
//! fn main() -> ExitCode {
//!     let runtime = match build_runtime(None) {
//!         Ok(runtime) => runtime,
//!         Err(error) => {
//!             let _ = writeln!(std::io::stderr(), "tokio runtime: {error}");
//!             return ExitCode::FAILURE;
//!         }
//!     };
//!     let code = run_until_exit(runtime, run);
//!     ExitCode::from(u8::try_from(code).unwrap_or(1))
//! }
//! ```
//!
//! Extracted from swarm-device-access/src/main.rs and src/run.rs (`Exit`) at 8b4ab3a3e417, with
//! swarm-scheduler-exporter/src/main.rs at c9e79afa8810 and awtrix-controller/src/main.rs and
//! src/run/mod.rs (`Exit` over an `ExitCode`) at afcd9ca26324, which differ only in the worker
//! count, the exit-code type and the startup message.

use std::future::Future;
use std::io;
use std::time::Duration;

use tokio::runtime::{Builder, Runtime};
use tokio::time::Instant;

/// How a program run by [`run_until_exit`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit<C> {
    /// The exit code, in the program's own type (an `i32` to narrow later, or an `ExitCode`).
    pub code: C,
    /// The shutdown deadline, once the shutdown started: what is left of it bounds the runtime's
    /// shutdown. `None` for a run that ended before it got that far, whose runtime gets no time.
    pub deadline: Option<Instant>,
}

impl<C> Exit<C> {
    /// An exit with no shutdown deadline: a run that ended before its shutdown started (a flag
    /// error, a failed startup).
    #[must_use]
    pub const fn early(code: C) -> Self {
        Self {
            code,
            deadline: None,
        }
    }
}

/// A multi-thread runtime with every driver enabled (I/O, so signals, and time), on
/// `worker_threads` workers, or tokio's default (one per CPU) for `None`.
///
/// # Errors
///
/// The runtime could not be built (the operating system refused a thread or the I/O driver), or
/// `worker_threads` is `Some(0)` ([`io::ErrorKind::InvalidInput`]), which tokio would panic on.
pub fn build_runtime(worker_threads: Option<usize>) -> io::Result<Runtime> {
    let mut builder = Builder::new_multi_thread();
    match worker_threads {
        Some(0) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker_threads must be at least 1",
            ));
        }
        Some(workers) => {
            builder.worker_threads(workers);
        }
        None => {}
    }
    builder.enable_all().build()
}

/// Runs the program `main` returns on `runtime` until it ends, then shuts the runtime down, giving
/// its remaining tasks (blocking ones included) at most what is left of the [`Exit`]'s deadline,
/// and nothing without one. Returns the exit code.
///
/// `main` is called on the runtime, so what it does at once (registering a signal handler, say)
/// has the runtime's context.
pub fn run_until_exit<C, F, Fut>(runtime: Runtime, main: F) -> C
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Exit<C>>,
{
    let exit = runtime.block_on(async move { main().await });
    let remaining = exit.deadline.map_or(Duration::ZERO, |deadline| {
        deadline.saturating_duration_since(Instant::now())
    });
    runtime.shutdown_timeout(remaining);
    exit.code
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    use super::*;

    #[test]
    fn the_worker_count_is_the_callers() {
        let runtime = build_runtime(Some(2)).unwrap();
        assert_eq!(runtime.metrics().num_workers(), 2);
        let error = build_runtime(Some(0)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(build_runtime(None).unwrap().metrics().num_workers() >= 1);
    }

    /// The time driver, and the I/O driver the signal handlers need.
    #[cfg(unix)]
    #[test]
    fn the_runtime_has_its_drivers() {
        use tokio::signal::unix::{SignalKind, signal};
        let runtime = build_runtime(Some(1)).unwrap();
        runtime.block_on(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            let _user1 = signal(SignalKind::user_defined1()).unwrap();
        });
    }

    #[test]
    fn the_exit_code_comes_back_in_the_programs_type() {
        let code = run_until_exit(build_runtime(Some(1)).unwrap(), || async { Exit::early(7) });
        assert_eq!(code, 7);
        let code = run_until_exit(build_runtime(Some(1)).unwrap(), || async {
            Exit::early(ExitCode::from(2))
        });
        assert_eq!(code, ExitCode::from(2));
    }

    /// A blocking task that outlives the deadline does not hold the exit up.
    #[test]
    fn the_shutdown_ends_at_the_deadline() {
        let started = std::time::Instant::now();
        let code = run_until_exit(build_runtime(Some(1)).unwrap(), || async {
            tokio::task::spawn_blocking(|| thread::sleep(Duration::from_secs(30)));
            Exit {
                code: 0,
                deadline: Some(Instant::now() + Duration::from_millis(100)),
            }
        });
        assert_eq!(code, 0);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );

        let started = std::time::Instant::now();
        run_until_exit(build_runtime(Some(1)).unwrap(), || async {
            tokio::task::spawn_blocking(|| thread::sleep(Duration::from_secs(30)));
            Exit::early(0)
        });
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    /// A blocking task that ends before the deadline is waited for.
    #[test]
    fn the_shutdown_waits_within_the_deadline() {
        let finished = Arc::new(AtomicBool::new(false));
        run_until_exit(build_runtime(Some(1)).unwrap(), || {
            let finished = Arc::clone(&finished);
            async move {
                tokio::task::spawn_blocking(move || {
                    thread::sleep(Duration::from_millis(200));
                    finished.store(true, Ordering::SeqCst);
                });
                Exit {
                    code: 0,
                    deadline: Some(Instant::now() + Duration::from_secs(30)),
                }
            }
        });
        assert!(finished.load(Ordering::SeqCst));
    }

    /// `main` runs in the runtime's context: `tokio::spawn` and the signal drivers work from it
    /// before the first await.
    #[test]
    fn main_is_called_on_the_runtime() {
        let code = run_until_exit(build_runtime(Some(1)).unwrap(), || {
            let task = tokio::spawn(async { 3 });
            async move { Exit::early(task.await.unwrap()) }
        });
        assert_eq!(code, 3);
    }
}
