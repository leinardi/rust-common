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

//! A `tracing` subscriber set up the way the projects log: tracing-subscriber's text format or
//! one flat JSON object per line, no ANSI escapes, RFC 3339 UTC timestamps, at a maximum level,
//! to the caller's writer. Which writer (stdout, stderr, a bounded lane), which level and format,
//! and whether the target and the JSON span fields are shown are the caller's; the crate parses no
//! configuration and prints nothing itself.
//!
//! - [`build`] makes the [`Dispatch`]; a test scopes it with `tracing::dispatcher::with_default`.
//! - [`install_global`] makes it the process's global default.
//! - `install_log_bridge` (feature `log-bridge`) routes the `log` facade into it.
//! - `testing` (feature `testing`): `Capture`, a writer that keeps what a test logged, and
//!   `set_default`, a scoped default that never loses a record to tracing's interest cache.
//!
//! Extracted from awtrix-controller/src/logger/mod.rs at afcd9ca26324 (target and span fields
//! shown, the `log` bridge with tracing-subscriber's `tracing-log` feature).
//! Extracted from swarm-rootless-docker/src/logger.rs at 3bb0fd8a615b (target and span fields
//! shown, no bridge).
//! Extracted from bitgauge/src/logger.rs at f9bb91eb5a50 (target and JSON span fields hidden).
//! The three built the same subscriber otherwise; their differences are [`Options`] and the
//! `log-bridge` feature.

use std::fmt;

use tracing::Dispatch;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;

#[cfg(feature = "testing")]
pub mod testing;

/// The two formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Format {
    /// tracing-subscriber's text lines: timestamp, level, the span context (`name{fields}:`), the
    /// target when shown, the message, then the fields as `key=value`.
    Text,
    /// One JSON object per line, the event's fields at the top level beside `timestamp`, `level`,
    /// `message` and `target`.
    Json,
}

/// What the subscriber shows beyond the timestamp, the level, the message and the fields. The
/// default is tracing-subscriber's own: both shown, as awtrix-controller and swarm-rootless-docker
/// log; bitgauge turns both off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    target: bool,
    span_fields: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self::new()
    }
}

impl Options {
    /// The target and the JSON span fields shown.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            target: true,
            span_fields: true,
        }
    }

    /// Whether each record shows its target (the module path, or the `target:` it was given):
    /// before the message in text, as `"target"` in JSON.
    #[must_use]
    pub const fn target(self, shown: bool) -> Self {
        Self {
            target: shown,
            ..self
        }
    }

    /// Whether a JSON record carries the spans it was logged in, as `"span"` (the current one)
    /// and `"spans"` (all of them). JSON only: the text format always prefixes the span context.
    #[must_use]
    pub const fn span_fields(self, shown: bool) -> Self {
        Self {
            span_fields: shown,
            ..self
        }
    }

    /// Whether the target is shown.
    #[must_use]
    pub const fn shows_target(self) -> bool {
        self.target
    }

    /// Whether a JSON record carries its spans.
    #[must_use]
    pub const fn shows_span_fields(self) -> bool {
        self.span_fields
    }
}

/// The subscriber, writing `format` records at `level` and above to `writer`, with `options`.
pub fn build<W>(
    writer: W,
    format: Format,
    level: impl Into<LevelFilter>,
    options: Options,
) -> Dispatch
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let builder = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_max_level(level.into())
        .with_ansi(false)
        .with_target(options.target);
    match format {
        Format::Text => Dispatch::new(builder.finish()),
        Format::Json => Dispatch::new(
            builder
                .json()
                .flatten_event(true)
                .with_current_span(options.span_fields)
                .with_span_list(options.span_fields)
                .finish(),
        ),
    }
}

/// Makes `dispatch` the process's global default.
///
/// # Errors
///
/// [`InstallError`] when a global default is already installed.
pub fn install_global(dispatch: Dispatch) -> Result<(), InstallError> {
    tracing::dispatcher::set_global_default(dispatch).map_err(|_| InstallError)
}

/// Routes records logged through the `log` facade to the `tracing` dispatcher, global or scoped,
/// with their own target, file and line. Install it once, before or after [`install_global`].
///
/// # Errors
///
/// [`InstallError`] when a `log` logger is already installed.
#[cfg(feature = "log-bridge")]
pub fn install_log_bridge() -> Result<(), InstallError> {
    tracing_log::LogTracer::init().map_err(|_| InstallError)
}

/// A global logger (a `tracing` default, or with `install_log_bridge` a `log` logger) was already
/// installed. The caller words its own message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InstallError;

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a global logger is already installed")
    }
}

impl std::error::Error for InstallError {}
