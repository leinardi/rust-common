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

//! The go-slog redacting logger: Go's text handler at debug, with the time, wrapped in a handler
//! that rewrites attributes before they are written. Temporary, behind the `redact-go-slog`
//! feature, and removed once the suites log through [`super::Logger`].
//!
//! The clock and the zone of the record times are the caller's; both suites pass the system's,
//! `Arc::new(clock::SystemClock)` and go-compat's `local_zone`, as their copies read them.

use go_slog::{Attr, Config, Format, Level, Logger, Now, PlainStyle, Sink, Value};
use jiff::tz::TimeZone;

use super::{REDACTED, Secret, redact_env};

impl From<Secret> for Value {
    fn from(_: Secret) -> Self {
        Self::Str(REDACTED.to_owned())
    }
}

/// A logger that redacts as the module describes. Clones are cheap and share the sink.
#[derive(Debug, Clone)]
pub struct RedactingLogger {
    inner: Logger,
}

impl RedactingLogger {
    /// Go's text handler at debug, with record times from `now` written in `zone`, writing to
    /// `sink`.
    #[must_use]
    pub fn new(sink: Sink, zone: TimeZone, now: Now) -> Self {
        Self {
            inner: Logger::new(
                Config {
                    format: Format::Text,
                    level: Level::DEBUG,
                    include_time: true,
                    // The text handler has no plain style; any value writes the same.
                    plain_style: PlainStyle::Quoted,
                },
                sink,
                zone,
                now,
            ),
        }
    }

    /// `Logger.With`, redacting `attrs` first.
    #[must_use]
    pub fn with_attrs(&self, attrs: &[Attr]) -> Self {
        Self {
            inner: self.inner.with_attrs(&redact_attrs(attrs)),
        }
    }

    /// `Logger.WithGroup`.
    #[must_use]
    pub fn with_group(&self, name: &str) -> Self {
        Self {
            inner: self.inner.with_group(name),
        }
    }

    /// Logs at debug.
    pub fn debug(&self, message: &str, attrs: &[Attr]) {
        self.inner.debug(message, &redact_attrs(attrs));
    }

    /// Logs at info.
    pub fn info(&self, message: &str, attrs: &[Attr]) {
        self.inner.info(message, &redact_attrs(attrs));
    }

    /// Logs at warn.
    pub fn warn(&self, message: &str, attrs: &[Attr]) {
        self.inner.warn(message, &redact_attrs(attrs));
    }

    /// Logs at error.
    pub fn error(&self, message: &str, attrs: &[Attr]) {
        self.inner.error(message, &redact_attrs(attrs));
    }
}

fn redact_attrs(attrs: &[Attr]) -> Vec<Attr> {
    attrs.iter().map(redact_attr).collect()
}

fn redact_attr(attr: &Attr) -> Attr {
    if attr.key == "secret" {
        return Attr::string(attr.key.clone(), REDACTED);
    }
    match &attr.value {
        Value::Strings(entries) if attr.key == "env" => {
            Attr::new(attr.key.clone(), Value::Strings(redact_env(entries)))
        }
        Value::Str(_)
        | Value::Int(_)
        | Value::Uint(_)
        | Value::Float(_)
        | Value::Bool(_)
        | Value::Duration(_)
        | Value::Time(_)
        | Value::Strings(_)
        | Value::NilStrings
        | Value::Stringer(_)
        | Value::Error(_)
        | Value::Nil
        | Value::Group(_) => attr.clone(),
    }
}

impl super::Log for RedactingLogger {
    fn log(&self, level: super::Level, message: &str, fields: &[(&str, &str)]) {
        let attrs: Vec<Attr> = fields
            .iter()
            .map(|(key, value)| Attr::string(*key, *value))
            .collect();
        match level {
            super::Level::Debug => Self::debug(self, message, &attrs),
            super::Level::Info => Self::info(self, message, &attrs),
            super::Level::Warn => Self::warn(self, message, &attrs),
            super::Level::Error => Self::error(self, message, &attrs),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use clock::FakeClock;
    use go_slog::{Attr, MemorySink, Value};
    use jiff::tz::TimeZone;

    use super::RedactingLogger;
    use crate::redact::{REDACTED, Secret, redact_env};

    fn clock() -> FakeClock {
        FakeClock::new("2026-10-11T12:00:00.5Z".parse().unwrap())
    }

    fn logger(sink: &MemorySink) -> RedactingLogger {
        RedactingLogger::new(Box::new(sink.clone()), TimeZone::UTC, Arc::new(clock()))
    }

    /// swarm-scheduler-exporter's Go harness: `TestRedactingLogger` (this project's Go suite
    /// has none).
    #[test]
    fn redacting_logger_keeps_secrets_out() {
        let sink = MemorySink::default();
        let log = logger(&sink).with_attrs(&[Attr::string("secret", "token-a")]);
        log.info(
            "bring-up",
            &[
                Attr::new("token", Secret::new("token-b")),
                Attr::strings("env", ["DOCKER_AUTH=token-c", "token-d"]),
                Attr::strings("nodes", ["manager-0", "worker-0"]),
            ],
        );
        let logged = sink.contents();

        for leaked in ["token-a", "token-b", "token-c", "token-d"] {
            assert!(
                !logged.contains(leaked),
                "the log leaks {leaked}:\n{logged}"
            );
        }
        for kept in ["DOCKER_AUTH=", "manager-0", "worker-0", "msg=bring-up"] {
            assert!(
                logged.contains(kept),
                "the log lost {kept}, which is not secret:\n{logged}"
            );
        }

        let formatted = format!(
            "{} {:?} {}",
            Secret::new("x"),
            Secret::new("x"),
            Secret::new("x")
        );
        assert!(
            !formatted.contains('x'),
            "a Secret formats as {formatted:?}"
        );
        assert_eq!(formatted.matches(REDACTED).count(), 3, "{formatted:?}");
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

    /// The record is Go's text handler's, with the caller's clock and zone; every other value
    /// passes untouched. A `secret` attribute logged under `with_group` is still redacted, while
    /// one nested in a group value is not inspected, as in Go.
    #[test]
    fn records_are_go_s_text_and_only_secrets_change() {
        let sink = MemorySink::default();
        let log = logger(&sink).with_attrs(&[Attr::string("envid", "1a2b3c4d")]);
        log.debug(
            "sweep: keeping environment",
            &[
                Attr::string("reason", "the current environment"),
                Attr::new("count", Value::Int(2)),
                Attr::new("names", Value::NilStrings),
                Attr::new("addr", Value::Stringer("127.0.0.1".to_owned())),
                Attr::new("secret", Value::Int(7)),
            ],
        );
        log.with_group("node").warn(
            "w",
            &[
                Attr::string("secret", "under-with-group"),
                Attr::new("g", Value::Group(vec![Attr::string("secret", "nested")])),
            ],
        );
        assert_eq!(
            sink.contents(),
            "time=2026-10-11T12:00:00.500Z level=DEBUG msg=\"sweep: keeping environment\" \
             envid=1a2b3c4d reason=\"the current environment\" count=2 names=[] \
             addr=127.0.0.1 secret=[redacted]\n\
             time=2026-10-11T12:00:00.500Z level=WARN msg=w envid=1a2b3c4d node.secret=[redacted] \
             node.g.secret=nested\n"
        );
    }
}
