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

//! Typed attribute values: the kinds of slog value a program logs, each rendered the way slog
//! renders that kind in each format.
//!
//! Extracted from swarm-device-access/src/logger/value.rs at 8b4ab3a3e417, the superset of
//! swarm-scheduler-exporter/src/logger/value.rs at c9e79afa8810 (it adds `Value::NilStrings` and
//! `Value::Stringer`).

use std::fmt;

use jiff::Timestamp;
use jiff::tz::Offset;

/// A key and a value.
#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    /// The key. slog's handlers write an empty key as `""`, and inline a group without one;
    /// the plain styles each have their own rule (see [`PlainStyle`](crate::PlainStyle)).
    pub key: String,
    /// The value.
    pub value: Value,
}

/// A time with its zone offset, as a Go `time.Time` carries its location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    /// The instant.
    pub timestamp: Timestamp,
    /// The offset the instant is written at.
    pub offset: Offset,
}

/// An attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A string.
    Str(String),
    /// A signed integer (Go's `int` and `int64`).
    Int(i64),
    /// An unsigned integer.
    Uint(u64),
    /// A float64.
    Float(f64),
    /// A bool.
    Bool(bool),
    /// A `time.Duration`, in nanoseconds: text as `Duration.String`, JSON as the integer.
    Duration(i64),
    /// A `time.Time`.
    Time(Time),
    /// A `[]string`: text as `[a b c]`, JSON as an array.
    Strings(Vec<String>),
    /// A nil `[]string`: `[]` in text, `null` in JSON.
    NilStrings,
    /// A `fmt.Stringer` whose Go type is a string, by its `String` text: as a string in the text
    /// format, unquoted in the plain one (`fmt.Fprint`), and as `json.Marshal` of the string in
    /// JSON.
    Stringer(String),
    /// An error, by its text.
    Error(String),
    /// A nil value (a nil error or `any`): `<nil>` in text, `null` in JSON.
    Nil,
    /// A group of attributes, `slog.Group`.
    Group(Vec<Attr>),
}

impl Attr {
    /// An attribute.
    pub fn new(key: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    /// A string attribute.
    pub fn string(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(key, Value::Str(value.into()))
    }

    /// A signed integer attribute.
    pub fn int(key: impl Into<String>, value: i64) -> Self {
        Self::new(key, Value::Int(value))
    }

    /// An unsigned integer attribute.
    pub fn uint(key: impl Into<String>, value: u64) -> Self {
        Self::new(key, Value::Uint(value))
    }

    /// A float attribute.
    pub fn float(key: impl Into<String>, value: f64) -> Self {
        Self::new(key, Value::Float(value))
    }

    /// A bool attribute.
    pub fn bool(key: impl Into<String>, value: bool) -> Self {
        Self::new(key, Value::Bool(value))
    }

    /// A duration attribute, in nanoseconds.
    pub fn duration(key: impl Into<String>, nanos: i64) -> Self {
        Self::new(key, Value::Duration(nanos))
    }

    /// A time attribute.
    pub fn time(key: impl Into<String>, timestamp: Timestamp, offset: Offset) -> Self {
        Self::new(key, Value::Time(Time { timestamp, offset }))
    }

    /// A list of strings.
    pub fn strings<I, S>(key: impl Into<String>, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::new(
            key,
            Value::Strings(values.into_iter().map(Into::into).collect()),
        )
    }

    /// A nil `[]string` attribute.
    pub fn nil_strings(key: impl Into<String>) -> Self {
        Self::new(key, Value::NilStrings)
    }

    /// A `fmt.Stringer` attribute, by its text.
    pub fn stringer(key: impl Into<String>, value: &dyn fmt::Display) -> Self {
        Self::new(key, Value::Stringer(value.to_string()))
    }

    /// An error attribute, by the error's text.
    pub fn error(key: impl Into<String>, error: &dyn fmt::Display) -> Self {
        Self::new(key, Value::Error(error.to_string()))
    }

    /// A nil attribute.
    pub fn nil(key: impl Into<String>) -> Self {
        Self::new(key, Value::Nil)
    }

    /// `slog.Group`: the attributes, with the empty groups among them removed, as
    /// `slog.GroupValue` does.
    pub fn group(key: impl Into<String>, attrs: Vec<Attr>) -> Self {
        Self::new(
            key,
            Value::Group(
                attrs
                    .into_iter()
                    .filter(|attr| !attr.is_empty_group())
                    .collect(),
            ),
        )
    }

    /// Whether this is a group without attributes, which slog drops.
    pub(crate) fn is_empty_group(&self) -> bool {
        matches!(&self.value, Value::Group(attrs) if attrs.is_empty())
    }

    /// Whether this is slog's empty `Attr{}`: no key and a nil value. Handlers elide it.
    pub(crate) fn is_empty(&self) -> bool {
        self.key.is_empty() && self.value == Value::Nil
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::Uint(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<Vec<String>> for Value {
    fn from(value: Vec<String>) -> Self {
        Self::Strings(value)
    }
}
