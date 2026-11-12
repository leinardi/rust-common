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

//! The untyped tree: what the YAML file and the environment say, before any key is typed. Every
//! value keeps its [`Origin`], the line and column the file wrote it at or the environment
//! variable that set it, so a problem can name it and a typing rule can depend on it.
//!
//! Extracted from bitgauge/src/config/raw.rs at 9be3b0d20765 (the tree and its `Display`), with
//! the location widened into an origin; the author relicenses it from GPL-3.0-or-later to MIT OR
//! Apache-2.0 for this repository.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

/// A value as the file or the environment gave it.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    /// A key written without a value, or `~` or `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An integer in the `i64` range.
    Int(i64),
    /// An integer outside the `i64` range, which a typed read refuses as too large.
    Wide(i128),
    /// A number with a fraction or an exponent, or a non-finite one (`.nan`, `.inf`).
    Float(f64),
    /// Text: a YAML string (quoted or plain) or an environment variable's value.
    Text(String),
    /// A sequence.
    List(Vec<Node>),
    /// A mapping.
    Map(Map),
}

/// The entries of a mapping, sorted by key, as the problem lines list them.
pub type Map = BTreeMap<String, Node>;

impl Raw {
    /// What kind of value this is, for a rule's text: `null`, `a boolean`, `a number`, `text`,
    /// `a list` or `a map`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "a boolean",
            Self::Int(_) | Self::Wide(_) | Self::Float(_) => "a number",
            Self::Text(_) => "text",
            Self::List(_) => "a list",
            Self::Map(_) => "a map",
        }
    }
}

impl fmt::Display for Raw {
    /// The value as a problem line shows it, in the shape of Go's `fmt.Sprint` of the decoded
    /// YAML: text unquoted, `[a b]` for a list, `map[k:v]` for a map and `<nil>` for a null inside
    /// them; unlike Go, a float is `f64`'s `Display` (never an exponent) and a null at the top is
    /// `null`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("null"),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Int(value) => write!(formatter, "{value}"),
            Self::Wide(value) => write!(formatter, "{value}"),
            Self::Float(value) => formatter.write_str(&crate::text::float(*value)),
            Self::Text(text) => formatter.write_str(text),
            Self::List(items) => {
                let mut shown = String::from("[");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        shown.push(' ');
                    }
                    let _ = write!(shown, "{}", Nested(&item.value));
                }
                shown.push(']');
                formatter.write_str(&shown)
            }
            Self::Map(entries) => {
                let mut shown = String::from("map[");
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        shown.push(' ');
                    }
                    let _ = write!(shown, "{key}:{}", Nested(&value.value));
                }
                shown.push(']');
                formatter.write_str(&shown)
            }
        }
    }
}

/// A value inside a list or a map, where Go writes a null as `<nil>`.
struct Nested<'raw>(&'raw Raw);

impl fmt::Display for Nested<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Raw::Null => formatter.write_str("<nil>"),
            other @ (Raw::Bool(_)
            | Raw::Int(_)
            | Raw::Wide(_)
            | Raw::Float(_)
            | Raw::Text(_)
            | Raw::List(_)
            | Raw::Map(_)) => write!(formatter, "{other}"),
        }
    }
}

/// Where the YAML file wrote a value: the line and column of its first character, from 1, as
/// serde-saphyr reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Location {
    /// The line, from 1.
    pub line: u64,
    /// The column, from 1.
    pub column: u64,
}

impl fmt::Display for Location {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.line, self.column)
    }
}

/// Where a value came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Origin {
    /// The YAML file, at this location.
    Yaml(Location),
    /// The environment variable of this name. A mapping the overlay created on the way to a
    /// variable's path carries that variable's name too.
    Environment(String),
}

impl Origin {
    /// Whether the value came from the environment, where every value is text.
    #[must_use]
    pub const fn is_environment(&self) -> bool {
        match self {
            Self::Environment(_) => true,
            Self::Yaml(_) => false,
        }
    }
}

/// A value with where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// The value.
    pub value: Raw,
    /// Where it came from.
    pub origin: Origin,
}

impl Node {
    /// A value with its origin.
    #[must_use]
    pub const fn new(value: Raw, origin: Origin) -> Self {
        Self { value, origin }
    }
}
