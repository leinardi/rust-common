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

//! Durations in jiff's friendly syntax (`500ms`, `1m30s`, `1h 30m`, `2 hours`), with two
//! optional extensions Go's koanf loader had: a bare `0`, and a leading day unit of exactly 24
//! hours (`30d`, `1.5d`, `7d12h`).
//!
//! The day count is turned into hours the way koanf's `expandDurationDays` does it (knadh/koanf
//! v2.4.1 `koanf.go`): the decimal text multiplied by 24 digit by digit, with no rounding, so
//! `1.5d` is `036.0h`. Since jiff refuses a unit written twice (`168h12h`), the day part and the
//! rest are parsed apart and added, which needs the day part to come first, written once, and
//! the rest to be non-negative, and jiff reads at most nine fraction digits (`1.0000000001d` is
//! refused where Go reads it). Go's `time.ParseDuration` also takes `0`, `+0` and `-0`, which
//! jiff refuses without a unit.
//!
//! Extracted from bitgauge/src/config/types.rs at 9be3b0d20765 (`parse_duration`, its error
//! text); the author relicenses it from GPL-3.0-or-later to MIT OR Apache-2.0 for this
//! repository. The day product follows `expandDurationDays` of knadh/koanf v2.4.1, Copyright
//! Kailash Nadh, MIT.

use std::fmt;

use jiff::SignedDuration;
use jiff::fmt::friendly::SpanParser;

/// Whether a bare `0` (and `+0`, `-0`) is the zero duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BareZero {
    /// Refused, as jiff refuses a number without a unit.
    Refused,
    /// The zero duration, as Go's `time.ParseDuration`.
    Zero,
}

/// Whether a leading day unit is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DayUnit {
    /// Refused, as jiff refuses days for a `SignedDuration`.
    Refused,
    /// `<n>d` first, `n` a decimal, is exactly `n × 24` hours, as koanf expands it.
    TwentyFourHours,
}

/// The duration syntax: jiff's friendly format plus the extensions chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DurationSyntax {
    /// Whether a bare `0` is the zero duration.
    pub bare_zero: BareZero,
    /// Whether a leading day unit is accepted.
    pub days: DayUnit,
}

impl DurationSyntax {
    /// jiff's friendly format with the extensions chosen.
    #[must_use]
    pub const fn new(bare_zero: BareZero, days: DayUnit) -> Self {
        Self { bare_zero, days }
    }

    /// jiff's friendly format alone.
    #[must_use]
    pub const fn friendly() -> Self {
        Self {
            bare_zero: BareZero::Refused,
            days: DayUnit::Refused,
        }
    }
}

/// Text that is not a duration in the syntax asked for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DurationError {
    text: String,
}

impl DurationError {
    /// The text refused.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for DurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} is not a duration such as 500ms, 10s or 2m",
            crate::text::quote(&self.text)
        )
    }
}

impl std::error::Error for DurationError {}

static PARSER: SpanParser = SpanParser::new();

/// Parses `text` as a duration in `syntax`.
///
/// # Errors
///
/// A [`DurationError`] naming the text when it is not a duration in that syntax.
pub fn parse_duration(text: &str, syntax: DurationSyntax) -> Result<SignedDuration, DurationError> {
    let refused = || DurationError {
        text: text.to_owned(),
    };
    let (negative, unsigned) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if syntax.bare_zero == BareZero::Zero && unsigned == "0" {
        return Ok(SignedDuration::ZERO);
    }
    if syntax.days == DayUnit::TwentyFourHours
        && let Some((hours, rest)) = leading_days(unsigned)
    {
        let days = PARSER
            .parse_duration(format!("{hours}h"))
            .map_err(|_| refused())?;
        let rest = rest.trim_start();
        let total = if rest.is_empty() {
            days
        } else {
            if rest.starts_with(['+', '-']) {
                return Err(refused());
            }
            let rest: SignedDuration = PARSER.parse_duration(rest).map_err(|_| refused())?;
            if rest.is_negative() {
                return Err(refused());
            }
            days.checked_add(rest).ok_or_else(refused)?
        };
        return if negative {
            total.checked_neg().ok_or_else(refused)
        } else {
            Ok(total)
        };
    }
    PARSER.parse_duration(text).map_err(|_| refused())
}

/// Splits a leading `<decimal>d` off `text`, the decimal matching koanf's
/// `[0-9]+(\.[0-9]*)?|\.[0-9]+`, and returns it as hours (koanf's digit-wise product by 24, a
/// trailing `.` dropped) with the text after the `d`.
fn leading_days(text: &str) -> Option<(String, &str)> {
    let bytes = text.as_bytes();
    let whole = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let mut end = whole;
    let mut fraction = 0;
    if bytes.get(end) == Some(&b'.') {
        fraction = bytes[end + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        end += 1 + fraction;
    }
    if (whole == 0 && fraction == 0) || bytes.get(end) != Some(&b'd') {
        return None;
    }
    let mut hours: Vec<u8> = b"00".iter().chain(&bytes[..end]).copied().collect();
    let mut carry = 0;
    for digit in hours.iter_mut().rev() {
        if *digit == b'.' {
            continue;
        }
        let product = u32::from(*digit - b'0') * 24 + carry;
        // The remainder of a division by 10 is one decimal digit.
        *digit = b'0' + u8::try_from(product % 10).unwrap_or(0);
        carry = product / 10;
    }
    if hours.last() == Some(&b'.') {
        hours.pop();
    }
    let hours = String::from_utf8(hours).ok()?;
    Some((hours, &text[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digit-wise product, against koanf's `expandDurationDays` output for the same inputs
    /// (a Go run of the v2.4.1 function: `024h`, `0720h`, `036.0h`, `12.0h`, `120.h`,
    /// `168h12h`, `239.76h`), the trailing `.` of `120.h` dropped for jiff.
    #[test]
    fn day_text_becomes_hours_as_koanf_writes_them() {
        for (text, hours, rest) in [
            ("1d", "024", ""),
            ("30d", "0720", ""),
            ("1.5d", "036.0", ""),
            (".5d", "12.0", ""),
            ("5.d", "120", ""),
            ("7d12h", "168", "12h"),
            ("9.99d", "239.76", ""),
        ] {
            assert_eq!(leading_days(text), Some((hours.to_owned(), rest)), "{text}");
        }
        for text in ["d", ".d", "1h", "1 d", "x1d", ""] {
            assert_eq!(leading_days(text), None, "{text}");
        }
    }
}
