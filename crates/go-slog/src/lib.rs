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

//! A Go `log/slog`-compatible logger: slog's `TextHandler` and `JSONHandler`, and the plain format
//! of the Go programs these crates replace, byte for byte (go1.26's slog).
//!
//! A [`Logger`] carries a minimum [`Level`], a [`Format`], whether the time is logged, a sink and
//! the clock and zone the record time is read from. `with_attrs` and `with_group` return a copy;
//! the original is unchanged. Each record is formatted whole, then written under one lock, so
//! records never interleave.
//!
//! The levels are slog's: debug -4, info 0, warn 4, error 8, and any value between (`INFO+1`).
//!
//! Where the Go programs differed, the caller chooses:
//!
//! - the plain format's [`PlainStyle`]: [`Quoted`](PlainStyle::Quoted) (one record, one line) or
//!   [`Raw`](PlainStyle::Raw) (text as `fmt.Fprint` gives it), in [`Config::plain_style`];
//! - how a level or format name is read: [`parse_level`] and [`parse_format`] fall back to info
//!   and text as Go's `parseLevel` and `newHandler` do, while [`parse_level_strict`] and
//!   [`parse_format_strict`] refuse any other value with a [`ParseError`];
//! - how the logger is reached: a [`LogHandle`], a cloned handle on one shared cell that
//!   [`LogHandle::configure`] swaps (Go's `logger.L()` without a process global), or the
//!   process-wide logger of [`global`], [`set_global`] and [`configure`].
//!
//! A program that bans reading the wall clock or the environment outside its own seams passes them
//! in: [`Logger::new`], [`LogHandle::stdout`] and [`configure`] take the clock ([`Now`]);
//! [`LogHandle::new`] and [`set_global`] take a [`Logger`] and so its clock, and
//! [`LogHandle::configure`] and [`LogHandle::configure_to`] keep the current logger's.
//! [`LogHandle::stdout`] and [`configure`] take `getenv` for the zone. The sink is the caller's
//! too, except where Go writes to stdout: [`LogHandle::stdout`], [`LogHandle::configure`],
//! [`configure`] and the default [`global`] write to the process's stdout
//! ([`LogHandle::configure_to`], [`Logger::new`] and [`set_global`] take a sink). Only [`global`]
//! before any [`configure`] or [`set_global`] reads the system clock and `TZ` itself, as Go's
//! default logger does.
//!
//! Extracted from swarm-device-access/src/logger/mod.rs at 8b4ab3a3e417 (`LogHandle`,
//! `Logger::clock`, `Config::parse`, the fallback parsers on `go_compat::strings::to_lower`) and
//! swarm-scheduler-exporter/src/logger/mod.rs at c9e79afa8810 (the process-wide logger, the strict
//! parsers and their `ParseError`).

mod plain_handler;
mod slog_handler;
mod value;

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use go_clock::{Clock, SystemClock};
use go_compat::{local_zone, strings, time_layout};
use jiff::tz::TimeZone;

use plain_handler::PlainHandler;
pub use plain_handler::PlainStyle;
use slog_handler::SlogHandler;
pub use value::{Attr, Time, Value};

/// A slog level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(pub i32);

impl Level {
    /// `slog.LevelDebug`.
    pub const DEBUG: Self = Self(-4);
    /// `slog.LevelInfo`.
    pub const INFO: Self = Self(0);
    /// `slog.LevelWarn`.
    pub const WARN: Self = Self(4);
    /// `slog.LevelError`.
    pub const ERROR: Self = Self(8);
}

impl fmt::Display for Level {
    /// slog's `Level.String`: `INFO`, `INFO+1`, `DEBUG-1`, `ERROR+4`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (base, delta) = if *self < Self::INFO {
            ("DEBUG", self.0 - Self::DEBUG.0)
        } else if *self < Self::WARN {
            ("INFO", self.0 - Self::INFO.0)
        } else if *self < Self::ERROR {
            ("WARN", self.0 - Self::WARN.0)
        } else {
            ("ERROR", self.0 - Self::ERROR.0)
        };
        if delta == 0 {
            formatter.write_str(base)
        } else {
            write!(formatter, "{base}{delta:+}")
        }
    }
}

/// The log format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// slog's `TextHandler`.
    Text,
    /// slog's `JSONHandler`.
    Json,
    /// The plain handler, in the [`PlainStyle`] of [`Config::plain_style`].
    Plain,
}

/// Go's `parseLevel`: a level name in any case (Go's `strings.ToLower`), `debug`, `info`, `warn`
/// or `warning`, and `error`, `fatal` or `panic` (all three read as error). Anything else reads
/// as info, as in Go; [`parse_level_strict`] refuses it instead.
#[must_use]
pub fn parse_level(value: &str) -> Level {
    parse_level_strict(value).unwrap_or(Level::INFO)
}

/// The format switch of Go's `newHandler`: `json`, `text` or `plain` in any case (Go's
/// `strings.ToLower`). Anything else is text, as in Go; [`parse_format_strict`] refuses it
/// instead.
#[must_use]
pub fn parse_format(value: &str) -> Format {
    parse_format_strict(value).unwrap_or(Format::Text)
}

/// The level names [`parse_level`] reads, with nothing else accepted: a name in any case (Go's
/// `strings.ToLower`), `debug`, `info`, `warn` or `warning`, and `error`, `fatal` or `panic` (all
/// three read as error).
///
/// # Errors
///
/// Any other value, where Go silently fell back to info:
/// `invalid log level "<value>" (want one of debug, info, warn, warning, error, fatal, panic)`,
/// the value as Rust's `{:?}` writes it.
pub fn parse_level_strict(value: &str) -> Result<Level, ParseError> {
    match strings::to_lower(value).as_str() {
        "debug" => Ok(Level::DEBUG),
        "info" => Ok(Level::INFO),
        "warn" | "warning" => Ok(Level::WARN),
        "error" | "fatal" | "panic" => Ok(Level::ERROR),
        _ => Err(ParseError {
            what: "level",
            value: value.to_owned(),
            accepted: "debug, info, warn, warning, error, fatal, panic",
        }),
    }
}

/// The format names [`parse_format`] reads, with nothing else accepted: `json`, `text` or
/// `plain`, in any case (Go's `strings.ToLower`).
///
/// # Errors
///
/// Any other value, where Go silently fell back to text:
/// `invalid log format "<value>" (want one of json, text, plain)`, the value as Rust's `{:?}`
/// writes it.
pub fn parse_format_strict(value: &str) -> Result<Format, ParseError> {
    match strings::to_lower(value).as_str() {
        "json" => Ok(Format::Json),
        "text" => Ok(Format::Text),
        "plain" => Ok(Format::Plain),
        _ => Err(ParseError {
            what: "format",
            value: value.to_owned(),
            accepted: "json, text, plain",
        }),
    }
}

/// A level or format name that [`parse_level_strict`] or [`parse_format_strict`] does not
/// accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    what: &'static str,
    value: String,
    accepted: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid log {} {:?} (want one of {})",
            self.what, self.value, self.accepted
        )
    }
}

impl std::error::Error for ParseError {}

/// Where the record time comes from.
pub type Now = Arc<dyn Clock>;

/// A sink every record is written to, whole, under one lock.
pub type Sink = Box<dyn Write + Send>;

/// The logger options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The format.
    pub format: Format,
    /// The minimum level written.
    pub level: Level,
    /// Whether records carry their time.
    pub include_time: bool,
    /// How [`Format::Plain`] writes its text; the other formats ignore it.
    pub plain_style: PlainStyle,
}

impl Config {
    /// The options Go's `Configure(format, level, includeTime)` reads from its settings, with Go's
    /// fallbacks ([`parse_format`], [`parse_level`]).
    #[must_use]
    pub fn parse(format: &str, level: &str, include_time: bool, plain_style: PlainStyle) -> Self {
        Self {
            format: parse_format(format),
            level: parse_level(level),
            include_time,
            plain_style,
        }
    }
}

/// What every copy of a logger shares.
struct Shared {
    level: Level,
    include_time: bool,
    plain_style: PlainStyle,
    sink: Mutex<Sink>,
    zone: TimeZone,
    now: Now,
}

/// The handler a logger formats with.
#[derive(Debug, Clone, PartialEq)]
enum Handler {
    Slog(SlogHandler),
    Plain(PlainHandler),
}

/// A logger. Clones are cheap and share the sink.
#[derive(Clone)]
pub struct Logger {
    shared: Arc<Shared>,
    handler: Arc<Handler>,
}

impl fmt::Debug for Logger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Logger")
            .field("level", &self.shared.level)
            .field("include_time", &self.shared.include_time)
            .field("handler", &self.handler)
            .finish_non_exhaustive()
    }
}

impl Logger {
    /// A logger writing to `sink`, with record times from `now` written in `zone`.
    #[must_use]
    pub fn new(config: Config, sink: Sink, zone: TimeZone, now: Now) -> Self {
        let handler = match config.format {
            Format::Text => Handler::Slog(SlogHandler::new(false, config.include_time)),
            Format::Json => Handler::Slog(SlogHandler::new(true, config.include_time)),
            Format::Plain => Handler::Plain(PlainHandler::new(config.plain_style)),
        };
        Self {
            shared: Arc::new(Shared {
                level: config.level,
                include_time: config.include_time,
                plain_style: config.plain_style,
                sink: Mutex::new(sink),
                zone,
                now,
            }),
            handler: Arc::new(handler),
        }
    }

    /// The zone record times are written in, Go's `time.Local`.
    #[must_use]
    pub fn zone(&self) -> &TimeZone {
        &self.shared.zone
    }

    /// The clock record times are read from.
    #[must_use]
    pub fn clock(&self) -> Now {
        Arc::clone(&self.shared.now)
    }

    /// The plain style the logger was configured with, whatever its format.
    #[must_use]
    pub fn plain_style(&self) -> PlainStyle {
        self.shared.plain_style
    }

    /// Whether a record at `level` is written.
    #[must_use]
    pub fn enabled(&self, level: Level) -> bool {
        level >= self.shared.level
    }

    /// `Logger.With`: a copy that adds `attrs` to every record. No attributes return the same
    /// logger.
    #[must_use]
    pub fn with_attrs(&self, attrs: &[Attr]) -> Self {
        if attrs.is_empty() {
            return self.clone();
        }
        let handler = match &*self.handler {
            Handler::Slog(handler) => Handler::Slog(handler.with_attrs(attrs)),
            Handler::Plain(handler) => Handler::Plain(handler.with_attrs(attrs)),
        };
        self.with_handler(handler)
    }

    /// `Logger.WithGroup`: a copy that qualifies the attributes that follow with `name`. An empty
    /// name returns the same logger (and the plain format also ignores a blank one).
    #[must_use]
    pub fn with_group(&self, name: &str) -> Self {
        if name.is_empty() {
            return self.clone();
        }
        let handler = match &*self.handler {
            Handler::Slog(handler) => Handler::Slog(handler.with_group(name)),
            Handler::Plain(handler) => Handler::Plain(handler.with_group(name)),
        };
        self.with_handler(handler)
    }

    fn with_handler(&self, handler: Handler) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            handler: Arc::new(handler),
        }
    }

    /// Logs at debug.
    pub fn debug(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::DEBUG, message, attrs);
    }

    /// Logs at info.
    pub fn info(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::INFO, message, attrs);
    }

    /// Logs at warn.
    pub fn warn(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::WARN, message, attrs);
    }

    /// Logs at error.
    pub fn error(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::ERROR, message, attrs);
    }

    /// Logs one record at `level`, if enabled. Empty groups among `attrs` are dropped, as
    /// `Record.Add` does. A failed write has nowhere to be reported and is ignored, as slog's
    /// `Logger` ignores its handler's error.
    pub fn log(&self, level: Level, message: &str, attrs: &[Attr]) {
        if !self.enabled(level) {
            return;
        }
        let timestamp = self.shared.now.now();
        let time = (!time_layout::is_go_zero_time(timestamp)).then(|| Time {
            timestamp,
            offset: self.shared.zone.to_offset(timestamp),
        });
        let attrs: Vec<Attr> = attrs
            .iter()
            .filter(|attr| !attr.is_empty_group())
            .cloned()
            .collect();
        let line = match &*self.handler {
            Handler::Slog(handler) => handler.format(time, level, message, &attrs),
            // The plain handler checks the time option itself; slog's handlers drop the time
            // through the time stripper.
            Handler::Plain(handler) => {
                let time = time.filter(|_| self.shared.include_time);
                handler.format(time, level, message, &attrs)
            }
        };
        // A poisoned lock is recovered: the sink holds no state a panic could leave
        // half-updated.
        let mut sink = self
            .shared
            .sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _ = sink.write_all(line.as_bytes());
        let _ = sink.flush();
    }
}

/// A logger every component logs through, Go's `logger.L()` without a process global. Clones
/// share one cell: [`LogHandle::configure`] and [`LogHandle::set`] swap the logger in it, and
/// every clone logs through the new one from its next record. A handle from
/// [`LogHandle::with_attrs`] adds its attributes to the current logger at each record, as Go's
/// `logger.L().With(...)` at the call site does.
#[derive(Clone)]
pub struct LogHandle {
    current: Arc<RwLock<Logger>>,
    attrs: Arc<[Attr]>,
}

impl fmt::Debug for LogHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LogHandle")
            .field("current", &self.logger())
            .field("attrs", &self.attrs)
            .finish()
    }
}

impl LogHandle {
    /// A handle on `logger`.
    #[must_use]
    pub fn new(logger: Logger) -> Self {
        Self {
            current: Arc::new(RwLock::new(logger)),
            attrs: Arc::from([]),
        }
    }

    /// Go's default before `Configure`: text at info, with the time from `now` in the local zone
    /// (resolved from `getenv("TZ")` as Go does), to stdout. `plain_style` is kept for a later
    /// [`LogHandle::configure`].
    #[must_use]
    pub fn stdout(
        now: Now,
        getenv: impl Fn(&str) -> Option<String>,
        plain_style: PlainStyle,
    ) -> Self {
        Self::new(Logger::new(
            Config {
                format: Format::Text,
                level: Level::INFO,
                include_time: true,
                plain_style,
            },
            Box::new(io::stdout()),
            local_zone::local_zone(getenv),
            now,
        ))
    }

    /// The current logger, with this handle's attributes.
    #[must_use]
    pub fn logger(&self) -> Logger {
        let current = self
            .current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        current.with_attrs(&self.attrs)
    }

    /// Go's `Set`: replaces the logger every clone logs through.
    pub fn set(&self, logger: Logger) {
        // A poisoned lock is recovered: the cell holds a whole logger either way.
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = logger;
    }

    /// Go's `Configure`: installs the logger `format`, `level` and `include_time` ask for, read
    /// with Go's fallbacks ([`Config::parse`]), writing to stdout, with the clock, the zone and
    /// the plain style of the current one.
    pub fn configure(&self, format: &str, level: &str, include_time: bool) {
        // One read of the current logger, so the style, the zone and the clock come from the
        // same one even if another clone sets a logger meanwhile.
        let (plain_style, zone, now) = {
            let current = self.current.read().unwrap_or_else(PoisonError::into_inner);
            (
                current.plain_style(),
                current.zone().clone(),
                current.clock(),
            )
        };
        self.set(Logger::new(
            Config::parse(format, level, include_time, plain_style),
            Box::new(io::stdout()),
            zone,
            now,
        ));
    }

    /// [`LogHandle::configure`] with options and a sink of the caller's: Go's
    /// `newHandler(output, ...)`. The clock and the zone are the current logger's.
    pub fn configure_to(&self, config: Config, sink: Sink) {
        let (zone, now) = {
            let current = self.current.read().unwrap_or_else(PoisonError::into_inner);
            (current.zone().clone(), current.clock())
        };
        self.set(Logger::new(config, sink, zone, now));
    }

    /// A handle on the same cell that adds `attrs` to every record. No attributes return a plain
    /// clone.
    #[must_use]
    pub fn with_attrs(&self, attrs: &[Attr]) -> Self {
        Self {
            current: Arc::clone(&self.current),
            attrs: self.attrs.iter().chain(attrs).cloned().collect(),
        }
    }

    /// Whether a record at `level` is written by the current logger.
    #[must_use]
    pub fn enabled(&self, level: Level) -> bool {
        self.logger().enabled(level)
    }

    /// Logs at debug.
    pub fn debug(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::DEBUG, message, attrs);
    }

    /// Logs at info.
    pub fn info(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::INFO, message, attrs);
    }

    /// Logs at warn.
    pub fn warn(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::WARN, message, attrs);
    }

    /// Logs at error.
    pub fn error(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::ERROR, message, attrs);
    }

    /// Logs one record at `level` through the current logger.
    pub fn log(&self, level: Level, message: &str, attrs: &[Attr]) {
        self.logger().log(level, message, attrs);
    }
}

/// The process-wide logger, set by [`configure`] or [`set_global`].
static GLOBAL: RwLock<Option<Logger>> = RwLock::new(None);

/// The process-wide logger. Before [`configure`] or [`set_global`] it is Go's default: text at
/// info, with the system clock's time in the local zone (resolved from the process's `TZ`), to
/// stdout.
#[must_use]
pub fn global() -> Logger {
    if let Some(logger) = GLOBAL
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        return logger.clone();
    }
    let mut global = GLOBAL.write().unwrap_or_else(PoisonError::into_inner);
    global
        .get_or_insert_with(|| {
            Logger::new(
                Config {
                    format: Format::Text,
                    level: Level::INFO,
                    include_time: true,
                    // Unused by the text format; configure replaces the whole logger.
                    plain_style: PlainStyle::Quoted,
                },
                Box::new(io::stdout()),
                local_zone::local_zone(|key| std::env::var(key).ok()),
                Arc::new(SystemClock),
            )
        })
        .clone()
}

/// Replaces the process-wide logger.
pub fn set_global(logger: Logger) {
    *GLOBAL.write().unwrap_or_else(PoisonError::into_inner) = Some(logger);
}

/// Builds the logger `config` asks for, writing to stdout with record times from `now` in the
/// local zone (resolved from `getenv("TZ")` as Go does), installs it as the process-wide logger
/// and returns it.
pub fn configure(config: Config, now: Now, getenv: impl Fn(&str) -> Option<String>) -> Logger {
    let logger = Logger::new(
        config,
        Box::new(io::stdout()),
        local_zone::local_zone(getenv),
        now,
    );
    set_global(logger.clone());
    logger
}

/// A sink that keeps what is written, for tests: clones share the buffer.
#[derive(Debug, Clone, Default)]
pub struct MemorySink {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl MemorySink {
    /// Everything written so far, as text.
    #[must_use]
    pub fn contents(&self) -> String {
        let buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&buffer).into_owned()
    }
}

impl Write for MemorySink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
