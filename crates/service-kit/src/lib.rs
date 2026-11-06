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

//! Process plumbing a service's `main` repeats: its build stamps, its runtime and the bound on that
//! runtime's shutdown, and its signals. Every message, exit code and default stays the caller's:
//! the crate prints nothing and picks no policy.
//!
//! - [`buildinfo!`] reads the version, commit and date stamps the build set in the environment, in
//!   the calling crate, with the caller's [`Defaults`] for a missing stamp and its rule for an
//!   empty one ([`EmptyStamp`]). Always available.
//! - `runtime` (feature `runtime`): `build_runtime` builds the multi-thread runtime,
//!   `run_until_exit` runs the program on it and bounds the runtime's shutdown by what is left of
//!   the program's deadline (`Exit`).
//! - `signal` (feature `runtime`, Unix): `shutdown_signal` and `lazy_shutdown_signal` for SIGINT
//!   and SIGTERM, `hangup_channel` for SIGHUP.
//!
//! Extracted from swarm-device-access/src/run.rs and src/main.rs at 8b4ab3a3e417,
//! swarm-scheduler-exporter/src/run.rs, src/main.rs and src/version.rs at c9e79afa8810,
//! awtrix-controller/src/main.rs at afcd9ca26324 and bitgauge/src/buildinfo.rs at 600fc3905200;
//! each module names the files it came from.

#[cfg(feature = "runtime")]
pub mod runtime;
#[cfg(all(feature = "runtime", unix))]
pub mod signal;
mod stamps;

pub use stamps::{BuildInfo, Defaults, EmptyStamp};
