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

//! Typed reads of a node: an integer, a number, a boolean, text, a list or a duration, under a
//! [`Typing`] profile. A profile starts from [`Typing::strict`], where a value is read only as
//! the type it was written as (an environment value, always text, parsed in the plainest
//! syntax), and each setter widens one rule the way a consumer's loader does.
//!
//! Every scalar read refuses a non-finite number first (`.nan`, `.inf`), whatever the type asked
//! for; the list read leaves that to the read of each item.
//! A whole float (`8.0`) is an integer under every profile; a fraction never is. A number given as
//! text is read by `str::parse::<f64>`, which does not take Go's hex floats or `_` separators.
//!
//! Extracted from bitgauge/src/config/decode.rs at 9be3b0d20765 (the scalar conversions and
//! their rule texts), widened with Go's loader conversions that runhold keeps (its difference
//! 14); the author relicenses it from GPL-3.0-or-later to MIT OR Apache-2.0 for this repository.
//! `IntegerSyntax::GoBaseZero` translates `ParseUint` and `underscoreOK` of Go 1.25's
//! `strconv/atoi.go`, Copyright The Go Authors, BSD-3-Clause.

use std::fmt;

use jiff::SignedDuration;

use crate::duration::{BareZero, DurationError, DurationSyntax, parse_duration};
use crate::text::{float, quote};
use crate::tree::{Node, Raw};

/// Whether text written in the YAML file is parsed as a number or a boolean. Environment values
/// are always parsed, since every one is text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum QuotedScalars {
    /// `max: "32"` in the file is an error for an integer key.
    Refused,
    /// `max: "32"` in the file is 32, as `BitGauge` reads it.
    Parsed,
}

/// The spellings of a boolean given as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BoolSpelling {
    /// `true` and `false` only.
    Exact,
    /// Go's `strconv.ParseBool`: `1`, `t`, `T`, `TRUE`, `true`, `True` and `0`, `f`, `F`,
    /// `FALSE`, `false`, `False`.
    GoParseBool,
}

/// The syntax of an integer given as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IntegerSyntax {
    /// Decimal digits with an optional sign (`str::parse::<i64>`).
    Decimal,
    /// Go's `strconv.ParseInt(text, 0, 64)`, as mapstructure reads a string into an integer: an
    /// optional sign, then `0x`, `0o`, `0b` or a leading `0` (octal) for the base, and `_` as a
    /// digit separator (`1_000`).
    GoBaseZero,
}

/// Whether an integer written in the YAML file is accepted where text is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IntegerText {
    /// `name: 42` is an error for a text key.
    Refused,
    /// `name: 42` is the text `42`, as Go's weakly typed decode writes it.
    Decimal,
}

/// Whether a scalar is accepted where a list is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScalarLists {
    /// `nodes: all` is an error for a list key.
    Refused,
    /// `nodes: all` is the one-element list `[all]`, as Go's weakly typed decode lifts it.
    Lifted,
}

/// The typing rules of one loader. Every read takes the [`Node`], so a rule can depend on where
/// the value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Typing {
    quoted_scalars: QuotedScalars,
    booleans: BoolSpelling,
    integers: IntegerSyntax,
    integer_text: IntegerText,
    scalar_lists: ScalarLists,
    durations: DurationSyntax,
}

impl Default for Typing {
    fn default() -> Self {
        Self::strict()
    }
}

/// Why a node could not be read as the type asked for. The `Display` is Rust wording; a caller
/// with rule texts of its own matches on the variant.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TypeError {
    /// The value is of another kind.
    #[error("expected {expected}, got {got}")]
    Expected {
        /// What the read wanted, such as `an integer`.
        expected: &'static str,
        /// What the value is ([`Raw::kind`]).
        got: &'static str,
    },
    /// A NaN or an infinity.
    #[error("invalid number: {} is not a finite number", float(*.0))]
    NotFinite(f64),
    /// Text that is not a number in the syntax asked for.
    #[error("invalid number: {}", quote(.0))]
    InvalidNumber(String),
    /// An integer outside the `i64` range.
    #[error("invalid number: {0} is too large")]
    TooLarge(i128),
    /// A number with a fraction where an integer is expected.
    #[error("invalid number: {} is not a whole number", float(*.0))]
    NotWhole(f64),
    /// Text that is not a boolean in the spellings asked for.
    #[error("invalid boolean: {} (want {})", quote(text), spellings(*spelling))]
    InvalidBoolean {
        /// The text given.
        text: String,
        /// The spellings accepted.
        spelling: BoolSpelling,
    },
    /// Text that is not a duration.
    #[error(transparent)]
    Duration(#[from] DurationError),
}

const fn spellings(spelling: BoolSpelling) -> &'static str {
    match spelling {
        BoolSpelling::Exact => "true or false",
        BoolSpelling::GoParseBool => "true, false, 1, 0, t or f",
    }
}

/// `i64::MAX + 1` as a float: a whole float below it in magnitude fits an `i64`.
const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

impl Typing {
    /// The narrowest profile: a value is read only as the type it was written as, a text value
    /// from the file is never parsed, an environment value is parsed in decimal or as exactly
    /// `true` or `false`, and a duration is jiff's friendly syntax alone.
    #[must_use]
    pub const fn strict() -> Self {
        Self {
            quoted_scalars: QuotedScalars::Refused,
            booleans: BoolSpelling::Exact,
            integers: IntegerSyntax::Decimal,
            integer_text: IntegerText::Refused,
            scalar_lists: ScalarLists::Refused,
            durations: DurationSyntax::friendly(),
        }
    }

    /// Whether the file's text is parsed as a number or a boolean.
    #[must_use]
    pub const fn quoted_scalars(mut self, rule: QuotedScalars) -> Self {
        self.quoted_scalars = rule;
        self
    }

    /// The spellings of a boolean given as text.
    #[must_use]
    pub const fn booleans(mut self, rule: BoolSpelling) -> Self {
        self.booleans = rule;
        self
    }

    /// The syntax of an integer given as text.
    #[must_use]
    pub const fn integers(mut self, rule: IntegerSyntax) -> Self {
        self.integers = rule;
        self
    }

    /// Whether the file's integer is text where text is expected.
    #[must_use]
    pub const fn integer_text(mut self, rule: IntegerText) -> Self {
        self.integer_text = rule;
        self
    }

    /// Whether a scalar is a one-element list where a list is expected.
    #[must_use]
    pub const fn scalar_lists(mut self, rule: ScalarLists) -> Self {
        self.scalar_lists = rule;
        self
    }

    /// The duration syntax; with [`BareZero::Zero`] the file's integer `0` is also the zero
    /// duration.
    #[must_use]
    pub const fn durations(mut self, syntax: DurationSyntax) -> Self {
        self.durations = syntax;
        self
    }

    /// Whether this node's text may be parsed as a number or a boolean.
    fn parses_text(self, node: &Node) -> bool {
        node.origin.is_environment() || self.quoted_scalars == QuotedScalars::Parsed
    }

    /// An integer: a whole number, or text in the integer syntax where text is parsed.
    ///
    /// # Errors
    ///
    /// A [`TypeError`] for anything else.
    pub fn integer(&self, node: &Node) -> Result<i64, TypeError> {
        finite(&node.value)?;
        match &node.value {
            Raw::Int(number) => Ok(*number),
            Raw::Text(text) if self.parses_text(node) => parse_integer(text, self.integers)
                .ok_or_else(|| TypeError::InvalidNumber(text.clone())),
            Raw::Wide(number) => Err(TypeError::TooLarge(*number)),
            Raw::Float(number) if number.fract() == 0.0 && number.abs() < I64_LIMIT =>
            {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "checked above: whole and within the i64 range"
                )]
                Ok(*number as i64)
            }
            Raw::Float(number) => Err(TypeError::NotWhole(*number)),
            Raw::Null | Raw::Bool(_) | Raw::Text(_) | Raw::List(_) | Raw::Map(_) => {
                Err(expected(&node.value, "an integer"))
            }
        }
    }

    /// A finite number, or text that parses as one where text is parsed.
    ///
    /// # Errors
    ///
    /// A [`TypeError`] for anything else.
    pub fn number(&self, node: &Node) -> Result<f64, TypeError> {
        finite(&node.value)?;
        match &node.value {
            Raw::Float(number) => Ok(*number),
            #[expect(
                clippy::cast_precision_loss,
                reason = "an integer for a float key, converted as Go converts it"
            )]
            Raw::Int(number) => Ok(*number as f64),
            #[expect(
                clippy::cast_precision_loss,
                reason = "an integer for a float key, converted as Go converts it"
            )]
            Raw::Wide(number) => Ok(*number as f64),
            Raw::Text(text) if self.parses_text(node) => text
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| TypeError::InvalidNumber(text.clone())),
            Raw::Null | Raw::Bool(_) | Raw::Text(_) | Raw::List(_) | Raw::Map(_) => {
                Err(expected(&node.value, "a number"))
            }
        }
    }

    /// A boolean, or text in the boolean spellings where text is parsed. A number is never a
    /// boolean.
    ///
    /// # Errors
    ///
    /// A [`TypeError`] for anything else.
    pub fn boolean(&self, node: &Node) -> Result<bool, TypeError> {
        finite(&node.value)?;
        match &node.value {
            Raw::Bool(flag) => Ok(*flag),
            Raw::Text(text) if self.parses_text(node) => parse_bool(text, self.booleans)
                .ok_or_else(|| TypeError::InvalidBoolean {
                    text: text.clone(),
                    spelling: self.booleans,
                }),
            Raw::Null
            | Raw::Int(_)
            | Raw::Wide(_)
            | Raw::Float(_)
            | Raw::Text(_)
            | Raw::List(_)
            | Raw::Map(_) => Err(expected(&node.value, "true or false")),
        }
    }

    /// Text, or the file's integer as its decimal text where the profile allows it. A boolean
    /// or a fraction is never text.
    ///
    /// # Errors
    ///
    /// A [`TypeError`] for anything else.
    pub fn text(&self, node: &Node) -> Result<String, TypeError> {
        finite(&node.value)?;
        match &node.value {
            Raw::Text(text) => Ok(text.clone()),
            Raw::Int(number) if self.integer_text == IntegerText::Decimal => Ok(number.to_string()),
            Raw::Wide(number) if self.integer_text == IntegerText::Decimal => {
                Ok(number.to_string())
            }
            Raw::Null
            | Raw::Bool(_)
            | Raw::Int(_)
            | Raw::Wide(_)
            | Raw::Float(_)
            | Raw::List(_)
            | Raw::Map(_) => Err(expected(&node.value, "text")),
        }
    }

    /// The items of a list, or a scalar as the only item where the profile lifts it. The items
    /// are read by the caller, each with the typed read of its own type.
    ///
    /// # Errors
    ///
    /// A [`TypeError`] for anything else (a null or a mapping is never lifted).
    pub fn list<'node>(&self, node: &'node Node) -> Result<Vec<&'node Node>, TypeError> {
        // No finite check here, as BitGauge's list read has none: `.nan` is "expected a list"
        // unless it is lifted, and then its item read refuses it.
        match &node.value {
            Raw::List(items) => Ok(items.iter().collect()),
            Raw::Bool(_) | Raw::Int(_) | Raw::Wide(_) | Raw::Float(_) | Raw::Text(_)
                if self.scalar_lists == ScalarLists::Lifted =>
            {
                Ok(vec![node])
            }
            Raw::Null
            | Raw::Bool(_)
            | Raw::Int(_)
            | Raw::Wide(_)
            | Raw::Float(_)
            | Raw::Text(_)
            | Raw::Map(_) => Err(expected(&node.value, "a list")),
        }
    }

    /// A duration: text in the profile's duration syntax, or the file's integer `0` where a
    /// bare zero is accepted. Any other number is refused (Go read an integer as nanoseconds).
    ///
    /// # Errors
    ///
    /// A [`TypeError`] for anything else.
    pub fn duration(&self, node: &Node) -> Result<SignedDuration, TypeError> {
        finite(&node.value)?;
        match &node.value {
            Raw::Text(text) => Ok(parse_duration(text, self.durations)?),
            Raw::Int(0) if self.durations.bare_zero == BareZero::Zero => Ok(SignedDuration::ZERO),
            Raw::Null
            | Raw::Bool(_)
            | Raw::Int(_)
            | Raw::Wide(_)
            | Raw::Float(_)
            | Raw::List(_)
            | Raw::Map(_) => Err(expected(&node.value, "a duration as text")),
        }
    }
}

/// A NaN or an infinity is refused for every read, before its type is looked at.
fn finite(value: &Raw) -> Result<(), TypeError> {
    match value {
        Raw::Float(number) if !number.is_finite() => Err(TypeError::NotFinite(*number)),
        Raw::Null
        | Raw::Bool(_)
        | Raw::Int(_)
        | Raw::Wide(_)
        | Raw::Float(_)
        | Raw::Text(_)
        | Raw::List(_)
        | Raw::Map(_) => Ok(()),
    }
}

const fn expected(value: &Raw, expected: &'static str) -> TypeError {
    TypeError::Expected {
        expected,
        got: value.kind(),
    }
}

fn parse_bool(text: &str, spelling: BoolSpelling) -> Option<bool> {
    match (spelling, text) {
        (BoolSpelling::Exact | BoolSpelling::GoParseBool, "true")
        | (BoolSpelling::GoParseBool, "1" | "t" | "T" | "TRUE" | "True") => Some(true),
        (BoolSpelling::Exact | BoolSpelling::GoParseBool, "false")
        | (BoolSpelling::GoParseBool, "0" | "f" | "F" | "FALSE" | "False") => Some(false),
        _ => None,
    }
}

fn parse_integer(text: &str, syntax: IntegerSyntax) -> Option<i64> {
    match syntax {
        IntegerSyntax::Decimal => text.parse().ok(),
        IntegerSyntax::GoBaseZero => parse_go_base_zero(text),
    }
}

/// Go's `strconv.ParseInt(text, 0, 64)` (go1.25 `strconv/atoi.go`, `ParseInt` over
/// `ParseUint`): a sign, then a base prefix (`0b`, `0o`, `0x` in any case with at least one more
/// character, else a leading `0` for octal), digits of that base, and `_` as a digit separator
/// where `underscoreOK` allows it.
fn parse_go_base_zero(text: &str) -> Option<i64> {
    let (negative, unsigned) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let magnitude = parse_go_uint_base_zero(unsigned)?;
    if negative {
        0_i64.checked_sub_unsigned(magnitude)
    } else {
        i64::try_from(magnitude).ok()
    }
}

/// Go's `ParseUint(text, 0, 64)`.
fn parse_go_uint_base_zero(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    let (base, digits): (u8, &[u8]) = if first == b'0' {
        match bytes.get(1).map(u8::to_ascii_lowercase) {
            Some(b'b') if bytes.len() >= 3 => (2, &bytes[2..]),
            Some(b'o') if bytes.len() >= 3 => (8, &bytes[2..]),
            Some(b'x') if bytes.len() >= 3 => (16, &bytes[2..]),
            _ => (8, &bytes[1..]),
        }
    } else {
        (10, bytes)
    };
    let mut underscores = false;
    let mut value: u64 = 0;
    for &byte in digits {
        let digit = match byte {
            b'_' => {
                underscores = true;
                continue;
            }
            b'0'..=b'9' => byte - b'0',
            _ if byte.is_ascii_alphabetic() => byte.to_ascii_lowercase() - b'a' + 10,
            _ => return None,
        };
        if digit >= base {
            return None;
        }
        value = value
            .checked_mul(u64::from(base))?
            .checked_add(u64::from(digit))?;
    }
    if underscores && !underscore_ok(text) {
        return None;
    }
    Some(value)
}

/// Go's `underscoreOK`: an underscore only right after a digit or the base prefix, and followed
/// by a digit.
fn underscore_ok(text: &str) -> bool {
    let mut bytes = text.as_bytes();
    if let Some((b'-' | b'+', rest)) = bytes.split_first() {
        bytes = rest;
    }
    // '^' the start, '0' a digit or the base prefix, '_' an underscore, '!' anything else.
    let mut saw = b'^';
    let mut index = 0;
    let mut hex = false;
    if bytes.len() >= 2
        && bytes[0] == b'0'
        && matches!(bytes[1].to_ascii_lowercase(), b'b' | b'o' | b'x')
    {
        index = 2;
        saw = b'0';
        hex = bytes[1].eq_ignore_ascii_case(&b'x');
    }
    for &byte in &bytes[index..] {
        if byte.is_ascii_digit() || (hex && matches!(byte.to_ascii_lowercase(), b'a'..=b'f')) {
            saw = b'0';
            continue;
        }
        if byte == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
            continue;
        }
        if saw == b'_' {
            return false;
        }
        saw = b'!';
    }
    saw != b'_'
}

impl fmt::Display for BoolSpelling {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(spellings(*self))
    }
}
