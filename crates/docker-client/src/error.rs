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
//!
//! A connection that could not be made reads as moby's client words it (moby client v0.6.2
//! `doRequest`, `connectionFailed`): a refused or timed-out dial, permission denied on the socket,
//! or a unix socket path that does not exist, each naming the daemon host as configured
//! ([`Config::daemon_host`](crate::Config::daemon_host)). docker-client's own calls word their
//! dial that way; an error from one of bollard's own calls is worded through [`Error::at_host`].
//! Ported from runhold/crates/runhold-docker/src/error.rs at `8e88406a` (`dial_failure`,
//! `connection_failed`, `io_cause`); texts from moby client v0.6.2 `doRequest`.
//!
//! Not reproduced: moby's text for a host name that does not resolve, and its catch-all
//! `error during connect: <url.Error>`. Those failures keep the I/O error's own text.

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::io;
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
    // A fresh client for negotiation could not be built from the `Config`.
    Connect(crate::ConnectError),
    // The connection to the daemon at `host` could not be made.
    Dial { host: String, source: io::Error },
}

impl Error {
    /// The kind of failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// bollard's error, unless this is [`bounded`]'s own timeout, a cancellation, or a dial
    /// failure (whose cause is in [`source`](StdError::source)).
    #[must_use]
    pub fn engine(&self) -> Option<&BollardError> {
        match &self.repr {
            Repr::Engine(error) => Some(error),
            Repr::Timeout(_) | Repr::Cancelled | Repr::Connect(_) | Repr::Dial { .. } => None,
        }
    }

    /// This error worded as moby's client words a failed dial to `host` (the daemon host as
    /// configured, [`Config::daemon_host`](crate::Config::daemon_host)), when it is one:
    /// an [`ErrorKind::Unavailable`] error from one of bollard's own calls whose cause is a
    /// refused or timed-out connection, permission denied on the socket, or, for a `unix://`
    /// host, a socket path that does not exist. Its [`source`](StdError::source) is then that
    /// I/O error. Any other error comes back unchanged, a dial failure of docker-client's own
    /// calls included (it already names its host), and so does a broken connection after the
    /// answer started.
    #[must_use]
    pub fn at_host(self, host: &str) -> Self {
        let Repr::Engine(engine) = self.repr else {
            return self;
        };
        let cause = (self.kind == ErrorKind::Unavailable)
            .then(|| dial_cause(&engine))
            .flatten()
            .filter(|cause| moby_dial_text(host, *cause).is_some());
        let Some(cause) = cause else {
            return Self {
                kind: self.kind,
                repr: Repr::Engine(engine),
            };
        };
        let source = if let BollardError::IOError { err } = engine {
            err
        } else {
            io::Error::new(cause, engine)
        };
        Self::dial(host.to_owned(), source)
    }

    pub(crate) fn dial(host: String, source: io::Error) -> Self {
        Self {
            kind: ErrorKind::Unavailable,
            repr: Repr::Dial { host, source },
        }
    }

    pub(crate) fn connect(error: crate::ConnectError) -> Self {
        match error {
            crate::ConnectError::Client(error) => Self::from(error),
            crate::ConnectError::Dial { host, source } => Self::dial(host, source),
            other @ (crate::ConnectError::Invalid(_) | crate::ConnectError::Unsupported(_)) => {
                Self {
                    kind: ErrorKind::Other,
                    repr: Repr::Connect(other),
                }
            }
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
            Repr::Connect(error) => error.fmt(formatter),
            Repr::Dial { host, source } => DialText(host, source).fmt(formatter),
        }
    }
}

// The `source` is bollard's own cause, not bollard's error: `Display` already carries bollard's
// text, so a chain walker does not print that twice; `engine()` returns the whole error. bollard's
// `IOError` is transparent and names no source of its own, so under it the source is the
// `io::Error` itself, which a caller needs to tell the failure's kind. The trade-off: its text is
// also `Display`'s, so a walker that prints every link (anyhow's `{:#}`) repeats it once. A dial
// failure's source is its `io::Error` too, repeated the same way only where moby has no text for
// the cause and `Display` falls back to the I/O error's.
impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match &self.repr {
            Repr::Engine(BollardError::IOError { err }) => Some(err),
            Repr::Engine(error) => error.source(),
            Repr::Dial { source, .. } => Some(source),
            Repr::Timeout(_) | Repr::Cancelled | Repr::Connect(_) => None,
        }
    }
}

/// The text of a failed dial to `host`: moby's when it has one for the cause, else the I/O error's.
pub(crate) struct DialText<'text>(pub(crate) &'text str, pub(crate) &'text io::Error);

impl fmt::Display for DialText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(host, source) = self;
        match moby_dial_text(host, source.kind()) {
            Some(text) => formatter.write_str(&text),
            None => source.fmt(formatter),
        }
    }
}

/// moby's `doRequest` for a dial to `host` that failed with `cause`: `connectionFailed` for a
/// refused or timed-out dial, the permission text, and the missing-socket text for a `unix://`
/// host. `None` for every other cause.
fn moby_dial_text(host: &str, cause: io::ErrorKind) -> Option<String> {
    if matches!(
        cause,
        io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
    ) {
        return Some(if host.is_empty() {
            "Cannot connect to the Docker daemon. Is the docker daemon running on this host?"
                .to_owned()
        } else {
            format!("Cannot connect to the Docker daemon at {host}. Is the docker daemon running?")
        });
    }
    if cause == io::ErrorKind::PermissionDenied {
        return Some(format!(
            "permission denied while trying to connect to the docker API at {host}"
        ));
    }
    if cause == io::ErrorKind::NotFound {
        let path = host.strip_prefix("unix://")?;
        return Some(format!(
            "failed to connect to the docker API at {host}; check if the path is correct and if \
             the daemon is running: dial unix {path}: connect: no such file or directory"
        ));
    }
    None
}

/// The kind of the I/O failure under one of bollard's errors: its own `IOError`, a socket that
/// does not exist, or, under a connect failure of bollard's connection pool (hyper-util's legacy
/// client, `is_connect`), the first `io::Error` in the cause chain, where hyper keeps the dial's
/// failure. A `HyperResponseError` is a failure after the connection was made (a body that broke
/// off, a keepalive that timed out) and is never a dial failure, whatever I/O error it carries.
fn dial_cause(error: &BollardError) -> Option<io::ErrorKind> {
    if let BollardError::IOError { err } = error {
        return Some(err.kind());
    }
    if matches!(error, BollardError::SocketNotFoundError(_)) {
        return Some(io::ErrorKind::NotFound);
    }
    let BollardError::HyperLegacyError { err } = error else {
        return None;
    };
    if !err.is_connect() {
        return None;
    }
    let mut cause = err.source();
    while let Some(current) = cause {
        if let Some(io) = current.downcast_ref::<io::Error>() {
            return Some(io.kind());
        }
        cause = current.source();
    }
    None
}
