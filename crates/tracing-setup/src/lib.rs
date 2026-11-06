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
//! - [`build_reloadable`] also returns a [`ReloadHandle`] that changes the format and the level
//!   while the dispatch is installed (a SIGHUP reload).
//! - [`Options::time`] turns the timestamp off, for a supervisor that stamps the lines.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Dispatch, Event, Metadata, Subscriber, span};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layered, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{Layer, Registry, reload};

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

/// What the subscriber shows beyond the level, the message and the fields. The
/// default is tracing-subscriber's own: both shown, as awtrix-controller and swarm-rootless-docker
/// log; bitgauge turns both off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    target: bool,
    span_fields: bool,
    time: bool,
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
            time: true,
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

    /// Whether each record starts with its RFC 3339 UTC timestamp (text) or carries `"timestamp"`
    /// (JSON). On by default; off for a process whose supervisor stamps the lines.
    #[must_use]
    pub const fn time(self, shown: bool) -> Self {
        Self {
            time: shown,
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

    /// Whether the timestamp is shown.
    #[must_use]
    pub const fn shows_time(self) -> bool {
        self.time
    }
}

/// The fmt layer for `format` and `options`, writing to `writer`.
fn fmt_layer<S, W>(writer: W, format: Format, options: Options) -> BoxedLayer<S>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(options.target);
    match (format, options.time) {
        (Format::Text, true) => Box::new(layer),
        (Format::Text, false) => Box::new(layer.without_time()),
        (Format::Json, time) => {
            let layer = layer
                .json()
                .flatten_event(true)
                .with_current_span(options.span_fields)
                .with_span_list(options.span_fields);
            if time {
                Box::new(layer)
            } else {
                Box::new(layer.without_time())
            }
        }
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
    Dispatch::new(
        tracing_subscriber::registry()
            .with(fmt_layer(writer, format, options))
            .with(level.into()),
    )
}

type BoxedLayer<S> = Box<dyn Layer<S> + Send + Sync + 'static>;
type TextSubscriber = Layered<Gate<BoxedLayer<Registry>>, Registry>;
type JsonSubscriber = Layered<Gate<BoxedLayer<TextSubscriber>>, TextSubscriber>;

/// A writer shared by the text layer and the JSON layer of a reloadable subscriber.
#[derive(Debug)]
struct Shared<W>(Arc<W>);

impl<'writer, W> MakeWriter<'writer> for Shared<W>
where
    W: MakeWriter<'writer>,
{
    type Writer = W::Writer;

    fn make_writer(&'writer self) -> Self::Writer {
        self.0.make_writer()
    }

    fn make_writer_for(&'writer self, meta: &Metadata<'_>) -> Self::Writer {
        self.0.make_writer_for(meta)
    }
}

/// Forwards everything to `inner` but the events, which it hands over only while the format it
/// stands for (`json`) is the active one. Span callbacks always reach `inner`, so both layers
/// keep every span's fields in their own representation (`FormattedFields<DefaultFields>` for
/// text, `FormattedFields<JsonFields>` for JSON), whichever format was active when the span was
/// created or recorded.
struct Gate<L> {
    inner: L,
    json: bool,
    active: Arc<AtomicBool>,
}

impl<S, L> Layer<S> for Gate<L>
where
    S: Subscriber,
    L: Layer<S>,
{
    fn on_register_dispatch(&self, subscriber: &Dispatch) {
        self.inner.on_register_dispatch(subscriber);
    }

    fn on_layer(&mut self, subscriber: &mut S) {
        self.inner.on_layer(subscriber);
    }

    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        self.inner.register_callsite(metadata)
    }

    fn enabled(&self, metadata: &Metadata<'_>, ctx: Context<'_, S>) -> bool {
        self.inner.enabled(metadata, ctx)
    }

    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_new_span(attrs, id, ctx);
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.inner.max_level_hint()
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        self.inner.on_record(id, values, ctx);
    }

    fn on_follows_from(&self, id: &span::Id, follows: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_follows_from(id, follows, ctx);
    }

    fn event_enabled(&self, event: &Event<'_>, ctx: Context<'_, S>) -> bool {
        self.inner.event_enabled(event, ctx)
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if self.active.load(Ordering::Acquire) == self.json {
            self.inner.on_event(event, ctx);
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_enter(id, ctx);
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_exit(id, ctx);
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        self.inner.on_close(id, ctx);
    }

    fn on_id_change(&self, old: &span::Id, new: &span::Id, ctx: Context<'_, S>) {
        self.inner.on_id_change(old, new, ctx);
    }
}

/// Like [`build`], for a subscriber whose format and level change while it is installed: the
/// [`Dispatch`] to install, and the [`ReloadHandle`] that reconfigures it. The options stay as
/// given.
///
/// Both formats are always built: a text layer and a JSON layer share the writer, and each keeps
/// the span fields in its own representation, as a span created under one format must render in
/// the other after a switch. The active format picks which of the two writes each event.
pub fn build_reloadable<W>(
    writer: W,
    format: Format,
    level: impl Into<LevelFilter>,
    options: Options,
) -> (Dispatch, ReloadHandle)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let writer = Arc::new(writer);
    let active = Arc::new(AtomicBool::new(format == Format::Json));
    let text = Gate {
        inner: fmt_layer(Shared(Arc::clone(&writer)), Format::Text, options),
        json: false,
        active: Arc::clone(&active),
    };
    let json = Gate {
        inner: fmt_layer(Shared(writer), Format::Json, options),
        json: true,
        active: Arc::clone(&active),
    };
    let (level_layer, level_handle) = reload::Layer::new(level.into());
    let dispatch = Dispatch::new(
        tracing_subscriber::registry()
            .with(text)
            .with(json)
            .with(level_layer),
    );
    let handle = ReloadHandle {
        active,
        level: level_handle,
    };
    (dispatch, handle)
}

/// Reconfigures a subscriber made by [`build_reloadable`]. Cheap to clone; every clone drives the
/// same subscriber, from any thread. The level is held weakly: once the [`Dispatch`] is gone,
/// every call fails with [`ReloadError`].
#[derive(Clone)]
pub struct ReloadHandle {
    active: Arc<AtomicBool>,
    level: reload::Handle<LevelFilter, JsonSubscriber>,
}

impl fmt::Debug for ReloadHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReloadHandle")
            .finish_non_exhaustive()
    }
}

impl ReloadHandle {
    /// Switches to `format` and `level`. The two swap one after the other, so a record logged
    /// between them sees one new and one old setting.
    ///
    /// # Errors
    ///
    /// [`ReloadError`] when the subscriber is gone.
    pub fn reconfigure(
        &self,
        format: Format,
        level: impl Into<LevelFilter>,
    ) -> Result<(), ReloadError> {
        self.set_format(format)?;
        self.set_level(level)
    }

    /// Switches to `level`, keeping the format.
    ///
    /// # Errors
    ///
    /// [`ReloadError`] when the subscriber is gone.
    pub fn set_level(&self, level: impl Into<LevelFilter>) -> Result<(), ReloadError> {
        self.level.reload(level.into()).map_err(ReloadError::new)
    }

    /// Switches to `format`, keeping the level.
    ///
    /// # Errors
    ///
    /// [`ReloadError`] when the subscriber is gone.
    pub fn set_format(&self, format: Format) -> Result<(), ReloadError> {
        self.level.with_current(|_| ()).map_err(ReloadError::new)?;
        self.active.store(format == Format::Json, Ordering::Release);
        Ok(())
    }
}

/// A [`ReloadHandle`] outlived its subscriber, or the subscriber's lock was poisoned.
#[derive(Debug)]
#[non_exhaustive]
pub struct ReloadError {
    source: reload::Error,
}

impl ReloadError {
    const fn new(source: reload::Error) -> Self {
        Self { source }
    }
}

impl fmt::Display for ReloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the log subscriber could not be reconfigured: {}",
            self.source
        )
    }
}

impl std::error::Error for ReloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
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
