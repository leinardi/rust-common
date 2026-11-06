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

//! The test-environment helpers the Go-ported suites share: Go's `-run` filter, the run's envid
//! and the labels that record its owner, the sweep's rules, the tool gate, git isolation for
//! scratch repositories, the implementation identity rule, and Go's `testing` and `context`
//! packages for a `harness = false` runner. What is a suite's own (its cluster, images, tests,
//! label names and variables) stays in the suite: it passes them in as configuration.
//!
//! Without features, the modules need no Docker, no network and no runtime, so their tests run in
//! a plain `cargo test`:
//!
//! - [`run_filter`]: Go's `-run` semantics, a pattern that selects nothing an error.
//! - [`Error`]: a message with Go's `%w` context chain and `errors.Join`.
//! - [`envid`]: [`envid::EnvIds`], an envid's bytes, label and resource-name prefix.
//! - [`sweep_rules`]: which abandoned runs a sweep removes, the owner (`Owner`, `parse_owner`,
//!   `current_pid_namespace`), the run's labels and its manual cleanup command.
//! - [`toolgate`]: [`toolgate::Gate`], an external tool found or the test skipped, or failed where
//!   the project's variable is `1`; [`repo_root!`].
//! - [`gitenv`]: [`gitenv::isolate`], a git command that cannot reach the repository being
//!   committed.
//! - [`identity`]: the answer to `-help` that tells a Go build from a Rust one, and
//!   [`identity::is_static_elf`].
//!
//! Features:
//!
//! - `async`: `context` (Go's `context.Context` on tokio) and `testing` (Go's `testing` package
//!   for an async runner: `TestHandle`, `Fatal`, `OrFatal`, `Outcome`, `Suite`, and the runner's
//!   `--run`, suite deadline, signals and tally), whose output is Go's verbose test output.
//! - `sync`: `runner`, the runner without async (`TestCase`, `main`, `run_selected`), which also
//!   takes libtest's flags.
//! - `redact`: `redact`, the harness's log with secrets redacted, on plain strings: `Secret`,
//!   `redact_env`, `Log` and `Logger`.
//! - `bollard` (implies `async` and `redact`): `host`, the host daemon's sweep, listing and
//!   cleanup of runs on bollard, and the PID liveness check.

#[cfg(feature = "async")]
pub mod context;
pub mod envid;
mod error;
pub mod gitenv;
#[cfg(feature = "bollard")]
pub mod host;
pub mod identity;
#[cfg(feature = "redact")]
pub mod redact;
pub mod run_filter;
#[cfg(feature = "sync")]
pub mod runner;
pub mod sweep_rules;
#[cfg(feature = "async")]
pub mod testing;
mod text;
pub mod toolgate;

pub use error::Error;
