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

//! The client's errors, worded as moby client v0.6.0 words them. The text is moby's; the
//! original cause (an `io::Error`, a `hyper::Error`, a `serde_json::Error`) is kept as the
//! [`std::error::Error::source`], so a caller can inspect it without the text changing.
//!
//! Extracted from swarm-scheduler-exporter/src/docker/error.rs at c9e79afa8810 (the causes).
//! Extracted from swarm-device-access/src/docker/error.rs at 8b4ab3a3e417 (`Canceled`, the
//! context's errors).

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use go_compat::quote::quote;
use hyper::StatusCode;

use crate::context::ContextError;

/// Why the settings could not be built: moby's message, after the caller's prefix where Go's
/// program added one (`docker client: …` for a `DOCKER_HOST` or `DOCKER_API_VERSION` moby
/// refuses, `configure TLS from DOCKER_CERT_PATH=<path>: …` for the TLS material).
#[derive(Debug)]
pub struct ConfigError {
    message: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl ConfigError {
    pub(crate) fn new(message: String) -> Self {
        Self {
            message,
            source: None,
        }
    }

    #[cfg(feature = "tls")]
    pub(crate) fn with_source(
        message: String,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self {
            message,
            source: Some(Box::new(source)),
        }
    }

    /// Prefixes the message, as Go's `fmt.Errorf("…: %w", err)` does.
    #[cfg(feature = "tcp")]
    pub(crate) fn context(self, prefix: &str) -> Self {
        Self {
            message: format!("{prefix}: {}", self.message),
            source: self.source,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for ConfigError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

/// What kind of failure an [`Error`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// moby's `errConnectionFailed`: no response, for a reason in the message.
    ConnectionFailed,
    /// The daemon answered with a status of 400 or above; [`Error::status`] has it.
    Response,
    /// The response body failed after the headers.
    Body,
    /// The response body is not the JSON the call expects.
    Decode,
    /// The call was refused before any request, as moby refuses an empty ID.
    InvalidParameter,
    /// The caller's deadline passed first: Go's `context.DeadlineExceeded`.
    DeadlineExceeded,
    /// The caller cancelled the call: Go's `context.Canceled`.
    Canceled,
}

/// A failed request. Clones share the cause.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    status: Option<StatusCode>,
    source: Option<Arc<dyn StdError + Send + Sync>>,
}

impl Error {
    fn with_kind(kind: ErrorKind, message: String) -> Self {
        Self {
            kind,
            message,
            status: None,
            source: None,
        }
    }

    /// moby's `errConnectionFailed`, with `message` as its text: a request that got no response.
    #[must_use]
    pub fn connection_failed(message: String) -> Self {
        Self::with_kind(ErrorKind::ConnectionFailed, message)
    }

    /// The daemon's answer with `status` (400 or above), with `message` as its text.
    #[must_use]
    pub fn response(status: StatusCode, message: String) -> Self {
        Self {
            status: Some(status),
            ..Self::with_kind(ErrorKind::Response, message)
        }
    }

    /// A body that failed after the headers, with `message` as its text.
    #[must_use]
    pub fn body(message: String) -> Self {
        Self::with_kind(ErrorKind::Body, message)
    }

    /// An answer that is not the JSON the call expects, with `message` as its text.
    #[must_use]
    pub fn decode(message: String) -> Self {
        Self::with_kind(ErrorKind::Decode, message)
    }

    /// A call refused before any request, with `message` as its text.
    #[must_use]
    pub fn invalid_parameter(message: String) -> Self {
        Self::with_kind(ErrorKind::InvalidParameter, message)
    }

    /// Go's `context.DeadlineExceeded`, which moby passes through undecorated.
    #[must_use]
    pub fn deadline_exceeded() -> Self {
        Self::context(ContextError::DeadlineExceeded)
    }

    /// The bare error of a done context, Go's `ctx.Err()`.
    #[must_use]
    pub fn context(done: ContextError) -> Self {
        Self::with_kind(Self::context_kind(done), done.text().to_owned())
    }

    /// The error of a request the done context cut short: net/http's `url.Error`, `<operation>
    /// "<url>": <context error>`, which moby passes through undecorated.
    pub(crate) fn context_request(done: ContextError, operation: &str, url: &str) -> Self {
        Self::with_kind(
            Self::context_kind(done),
            format!("{operation} {}: {}", quote(url), done.text()),
        )
    }

    const fn context_kind(done: ContextError) -> ErrorKind {
        match done {
            ContextError::Canceled => ErrorKind::Canceled,
            ContextError::DeadlineExceeded => ErrorKind::DeadlineExceeded,
        }
    }

    /// Keeps `source` as the cause, without changing the Go-compatible message. Clones share it.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self {
        self.source = Some(Arc::from(source.into()));
        self
    }

    /// containerd's `errdefs.IsNotFound` of a moby error: the daemon answered 404.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.kind == ErrorKind::Response && self.status == Some(StatusCode::NOT_FOUND)
    }

    /// The kind of failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The daemon's status, for a [`ErrorKind::Response`].
    #[must_use]
    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}
