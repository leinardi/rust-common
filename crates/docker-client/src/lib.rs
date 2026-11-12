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

//! Small shared plumbing over [bollard]: what a program that talks to a Docker engine through it
//! needs around the SDK, and the few calls bollard cannot make as a program needs them.
//!
//! * [`Config`] and [`connect`]: the engine's address (a unix socket, or `DOCKER_HOST` and the
//!   `DOCKER_*` variables through [`Config::from_env`]), a fixed API version, a `User-Agent` and
//!   bollard's response-header timeout, which bollard has no setter or reader for.
//! * [`bounded`]: a timeout around a whole call. bollard's own timeout covers only the wait for the
//!   response headers, so a body that stalls after them would hang a call forever.
//! * [`Error`] and [`ErrorKind`]: what a program acts on (not found, conflict, timeout,
//!   unavailable), with bollard's error kept as the cause.
//! * [`pump_stream`]: a bollard stream (the events, a log) as a channel, fed by a task that stops
//!   with its receiver or its [`CancellationToken`].
//! * [`events_established`]: the events stream, returned once the engine accepted the subscription.
//! * `attach` (feature): a container's output pump and its exit code (`wait` returns once the engine holds the waiter).
//! * `exec` (feature): a command run in a container, its standard streams one bidirectional
//!   connection, and `demux`, which splits an output stream by the framing the caller knows.
//! * `test_util` (feature `test-util`): a fake engine on a unix socket.
//!
//! Extracted from swarm-rootless-docker/src/docker/mod.rs at e0961f77655f (the call bound, the
//! error mapping, the connection, and the attach and wait pumps) and
//! swarm-rootless-docker/src/docker/tests.rs at e0961f77655f (the fake engine that stalls in the
//! body).

#[cfg(feature = "attach")]
pub mod attach;
mod config;
#[cfg(feature = "exec")]
pub mod demux;
mod error;
#[cfg(feature = "exec")]
pub mod exec;
#[cfg(test)]
#[cfg(feature = "exec")]
mod exec_tests;
mod pump;
mod stream;
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;
#[cfg(test)]
mod tests;

pub use bollard;
pub use config::{Config, ConnectError, connect, negotiate};
pub use error::{Error, ErrorKind, bounded};
pub use pump::pump_stream;
pub use stream::{EventStream, events_established};
pub use tokio_util::sync::CancellationToken;
