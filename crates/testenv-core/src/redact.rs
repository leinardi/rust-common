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

//! The harness's log (the sweep's decisions), with secrets kept out of it, and no logging
//! framework behind it: plain strings in, `level=INFO msg=... key=value` lines out, through a
//! writer the caller supplies.
//!
//! What is redacted: a field keyed `secret`, the values of a comma-joined `KEY=value` list keyed
//! `env`, a [`Secret`] formatted anywhere, and the values (not
//! the names) of `KEY=value` lists passed through [`redact_env`]. This harness logs no secret
//! today; the rule stays so that a later one (a registry credential, say) cannot reach the log by
//! accident.
//!
//! The sweep logs through the [`Log`] trait, which [`Logger`] implements. The earlier go-slog
//! `RedactingLogger` (Go's text handler byte for byte) is kept, unchanged, behind the temporary
//! `redact-go-slog` feature, and implements [`Log`] too.
//!
//! Extracted from swarm-device-access/testenv/src/redact.rs at 8b4ab3a3e417 and
//! swarm-scheduler-exporter/testenv/src/redact.rs at c9e79afa8810.

use std::fmt;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::Arc;

#[cfg(feature = "redact-go-slog")]
mod go_slog_impl;
#[cfg(feature = "redact-go-slog")]
pub use go_slog_impl::RedactingLogger;

/// What a redacted value is written as.
pub const REDACTED: &str = "[redacted]";

/// A value that must never reach a log or an error message. `Display` and `Debug` both write
/// [`REDACTED`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps `value`.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value itself, for the one place that needs it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED)
    }
}

/// `KEY=value` entries with every value replaced by [`REDACTED`]; an entry without `=` is
/// replaced whole.
#[must_use]
pub fn redact_env(env: &[String]) -> Vec<String> {
    env.iter()
        .map(|entry| match entry.split_once('=') {
            Some((key, _)) => format!("{key}={REDACTED}"),
            None => REDACTED.to_owned(),
        })
        .collect()
}

/// The severity of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Decisions kept quiet by default.
    Debug,
    /// Normal events.
    Info,
    /// Something went wrong that the run survives.
    Warn,
    /// Something failed.
    Error,
}

impl Level {
    /// `DEBUG`, `INFO`, `WARN` or `ERROR`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// Where the sweep logs: a message and plain string fields.
pub trait Log: Send + Sync {
    /// Writes one record.
    fn log(&self, level: Level, message: &str, fields: &[(&str, &str)]);

    /// Logs at debug.
    fn debug(&self, message: &str, fields: &[(&str, &str)]) {
        self.log(Level::Debug, message, fields);
    }

    /// Logs at info.
    fn info(&self, message: &str, fields: &[(&str, &str)]) {
        self.log(Level::Info, message, fields);
    }

    /// Logs at warn.
    fn warn(&self, message: &str, fields: &[(&str, &str)]) {
        self.log(Level::Warn, message, fields);
    }

    /// Logs at error.
    fn error(&self, message: &str, fields: &[(&str, &str)]) {
        self.log(Level::Error, message, fields);
    }
}

type Writer = Arc<dyn Fn(&str) + Send + Sync>;

/// A logger that redacts as the module describes and writes one line per record, without a
/// trailing newline, to its writer. Clones are cheap and share the writer.
#[derive(Clone)]
pub struct Logger {
    writer: Writer,
    min_level: Level,
    fields: Vec<(String, String)>,
}

impl fmt::Debug for Logger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Logger")
            .field("min_level", &self.min_level)
            .finish_non_exhaustive()
    }
}

impl Logger {
    /// A logger at debug that hands each line to `writer`.
    #[must_use]
    pub fn new(writer: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            writer: Arc::new(writer),
            min_level: Level::Debug,
            fields: Vec::new(),
        }
    }

    /// A logger at debug that writes each line to standard error.
    #[must_use]
    pub fn stderr() -> Self {
        Self::new(|line| {
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        })
    }

    /// This logger dropping records below `level`.
    #[must_use]
    pub fn with_min_level(mut self, level: Level) -> Self {
        self.min_level = level;
        self
    }

    /// This logger with `fields` on every record, redacted as any other.
    #[must_use]
    pub fn with_fields(mut self, fields: &[(&str, &str)]) -> Self {
        self.fields.extend(
            fields
                .iter()
                .map(|(name, text)| ((*name).to_owned(), (*text).to_owned())),
        );
        self
    }
}

impl Log for Logger {
    fn log(&self, level: Level, message: &str, fields: &[(&str, &str)]) {
        if level < self.min_level {
            return;
        }
        let mut line = format!("level={} msg={}", level.as_str(), value(message));
        let own = self
            .fields
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()));
        for (key, text) in own.chain(fields.iter().copied()) {
            let redacted = match key {
                "secret" => REDACTED.to_owned(),
                // A comma-joined `KEY=value` list: only the names survive, as through
                // `redact_env` (an element without `=`, a split value included, goes whole).
                "env" => {
                    redact_env(&text.split(',').map(str::to_owned).collect::<Vec<_>>()).join(",")
                }
                _ => text.to_owned(),
            };
            let _ = write!(line, " {key}={}", value(&redacted));
        }
        (self.writer)(&line);
    }
}

/// `text` as is when it is a bare word, else quoted.
fn value(text: &str) -> String {
    let bare = !text.is_empty()
        && text
            .chars()
            .all(|ch| !ch.is_whitespace() && !ch.is_control() && ch != '"' && ch != '=');
    if bare {
        text.to_owned()
    } else {
        format!("{text:?}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{Level, Log, Logger, REDACTED, Secret, redact_env};

    fn logger() -> (Logger, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let log = Logger::new(move |line| sink.lock().unwrap().push(line.to_owned()));
        (log, lines)
    }

    #[test]
    fn secrets_stay_out_of_the_log() {
        let (log, lines) = logger();
        let log = log.with_fields(&[("secret", "token-a")]);
        // Not passed through `redact_env` first: the `env` key alone redacts it.
        let env = "DOCKER_AUTH=token-c,token-d";
        log.info(
            "bring-up",
            &[
                ("token", &Secret::new("token-b").to_string()),
                ("env", env),
                ("nodes", "manager-0 worker-0"),
            ],
        );
        let logged = lines.lock().unwrap().join("\n");
        for leaked in ["token-a", "token-b", "token-c", "token-d"] {
            assert!(!logged.contains(leaked), "leaks {leaked}: {logged}");
        }
        assert_eq!(
            logged,
            "level=INFO msg=bring-up secret=[redacted] token=[redacted] \
             env=\"DOCKER_AUTH=[redacted],[redacted]\" nodes=\"manager-0 worker-0\""
        );
    }

    #[test]
    fn secret_formats_redacted() {
        let formatted = format!("{} {:?}", Secret::new("x"), Secret::new("x"));
        assert_eq!(formatted, format!("{REDACTED} {REDACTED}"));
        assert_eq!(Secret::new("x").expose(), "x");
        assert_eq!(
            redact_env(&["A=1".to_owned(), "B=".to_owned(), "C".to_owned()]),
            [
                format!("A={REDACTED}"),
                format!("B={REDACTED}"),
                REDACTED.to_owned()
            ]
        );
    }

    #[test]
    fn levels_filter_and_values_quote() {
        let (log, lines) = logger();
        let log = log.with_min_level(Level::Warn);
        log.info("dropped", &[]);
        log.warn("kept it", &[("err", ""), ("q", "a\"b")]);
        assert_eq!(
            *lines.lock().unwrap(),
            [r#"level=WARN msg="kept it" err="" q="a\"b""#]
        );
    }
}
