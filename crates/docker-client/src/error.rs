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

//! The error of a Docker call, sorted into the kinds a program acts on, and the whole-call bound.
//!
//! Extracted from swarm-rootless-docker/src/docker/mod.rs at e0961f77655f (`docker_error`,
//! `bounded_by`).

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use bollard::errors::Error as BollardError;

/// What a program does with a failed call; everything else about it is in the [`Error`]'s text and
/// cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The engine answered 404: the container, image or network is not there.
    NotFound,
    /// The engine answered 409: the name is taken, or the state refuses the call.
    Conflict,
    /// No complete answer in time: [`bounded`]'s timeout, or bollard's response-header timeout.
    Timeout,
    /// The engine could not be reached, or the connection broke mid-call: its socket is gone,
    /// refused, or reset.
    Unavailable,
    /// The caller's [`CancellationToken`](crate::CancellationToken) fired before the call was
    /// established. Not a failure of the engine: tell it from `Timeout` to shut down quietly.
    Cancelled,
    /// Anything else, an engine answer included (500, a body that does not decode). A `304 Not
    /// Modified` (start of a running container, stop of a stopped one) is not an error here:
    /// bollard counts it as a success, so the call returns `Ok`.
    Other,
}

/// A failed Docker call.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    repr: Repr,
}

#[derive(Debug)]
enum Repr {
    Engine(BollardError),
    Timeout(Duration),
    Cancelled,
}

impl Error {
    /// The kind of failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// bollard's error, unless this is [`bounded`]'s own timeout.
    #[must_use]
    pub fn engine(&self) -> Option<&BollardError> {
        match &self.repr {
            Repr::Engine(error) => Some(error),
            Repr::Timeout(_) | Repr::Cancelled => None,
        }
    }

    pub(crate) fn cancelled() -> Self {
        Self {
            kind: ErrorKind::Cancelled,
            repr: Repr::Cancelled,
        }
    }

    pub(crate) fn timeout(after: Duration) -> Self {
        Self {
            kind: ErrorKind::Timeout,
            repr: Repr::Timeout(after),
        }
    }
}

impl From<BollardError> for Error {
    fn from(error: BollardError) -> Self {
        Self {
            kind: kind_of(&error),
            repr: Repr::Engine(error),
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotFound => "not found",
            Self::Conflict => "conflict",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
            Self::Other => "other",
        })
    }
}

fn kind_of(error: &BollardError) -> ErrorKind {
    if let BollardError::DockerResponseServerError { status_code, .. } = error {
        return match status_code {
            404 => ErrorKind::NotFound,
            409 => ErrorKind::Conflict,
            _ => ErrorKind::Other,
        };
    }
    if matches!(error, BollardError::RequestTimeoutError) {
        ErrorKind::Timeout
    } else if matches!(
        error,
        BollardError::IOError { .. }
            | BollardError::HyperResponseError { .. }
            | BollardError::HyperLegacyError { .. }
            | BollardError::SocketNotFoundError(_)
    ) {
        ErrorKind::Unavailable
    } else {
        ErrorKind::Other
    }
}

/// Runs `call` for at most `timeout`, from the request to the end of the response body.
///
/// bollard's own timeout stops at the response headers, so a body that stalls after them would
/// hang the call.
///
/// # Errors
///
/// [`ErrorKind::Timeout`] when `timeout` passes first (the call is dropped), else the call's error
/// sorted into its kind.
pub async fn bounded<T>(
    timeout: Duration,
    call: impl Future<Output = Result<T, BollardError>>,
) -> Result<T, Error> {
    match tokio::time::timeout(timeout, call).await {
        Ok(result) => result.map_err(Error::from),
        Err(_elapsed) => Err(Error::timeout(timeout)),
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.repr {
            Repr::Engine(error) => error.fmt(formatter),
            Repr::Timeout(after) => write!(formatter, "no complete answer within {after:?}"),
            Repr::Cancelled => formatter.write_str("cancelled"),
        }
    }
}

// The `source` is bollard's own cause (the `io::Error` under an `IOError`), not bollard's error:
// `Display` already carries that one's text, so a chain walker does not print it twice. `engine()`
// returns the whole error.
impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match &self.repr {
            Repr::Engine(error) => error.source(),
            Repr::Timeout(_) | Repr::Cancelled => None,
        }
    }
}
