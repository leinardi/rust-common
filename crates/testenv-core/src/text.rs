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

//! The harness's own small text helpers, in place of a dependency on go-compat: quoting for
//! messages and the suite's duration variables (`90s`, `1h30m`, `1.5h`).

use std::time::Duration;

/// `text` in double quotes, escaped as Rust's `{:?}`.
pub(crate) fn quote(text: &str) -> String {
    format!("{text:?}")
}

/// Nanoseconds as a [`Duration`] (negative as zero), written as `Duration`'s `Debug`.
pub(crate) fn format_nanos(nanos: i64) -> String {
    format!("{:?}", Duration::from_nanos(nanos.max(0).unsigned_abs()))
}

const UNITS: [(&str, u128); 8] = [
    ("ns", 1),
    ("us", 1_000),
    ("\u{b5}s", 1_000),
    ("\u{3bc}s", 1_000),
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
];

/// A signed sequence of decimal numbers, each with an optional fraction and a unit (`ns`, `us`,
/// `\u{b5}s`, `ms`, `s`, `m`, `h`), such as `300ms`, `-1.5h` or `2h45m`, in nanoseconds; a bare
/// `0` is accepted. Fractions beyond a nanosecond are truncated.
///
/// # Errors
///
/// `time: invalid duration "<value>"` for anything else, or a value outside `i64`.
pub(crate) fn parse_duration(value: &str) -> Result<i64, String> {
    let invalid = || format!("time: invalid duration {}", quote(value));
    let (negative, mut rest) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    if rest == "0" {
        return Ok(0);
    }
    if rest.is_empty() {
        return Err(invalid());
    }
    let mut total: u128 = 0;
    while !rest.is_empty() {
        let number_end = rest
            .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
            .unwrap_or(rest.len());
        let (number, after) = rest.split_at(number_end);
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if whole.is_empty() && fraction.is_empty() || fraction.contains('.') {
            return Err(invalid());
        }
        let unit_end = after
            .find(|ch: char| ch.is_ascii_digit() || ch == '.')
            .unwrap_or(after.len());
        let (unit, next) = after.split_at(unit_end);
        let scale = UNITS
            .iter()
            .find(|(name, _)| *name == unit)
            .map(|(_, scale)| *scale)
            .ok_or_else(invalid)?;
        let digits = |text: &str| {
            text.bytes().try_fold(0_u128, |acc, byte| {
                acc.checked_mul(10)?.checked_add(u128::from(byte - b'0'))
            })
        };
        let whole = if whole.is_empty() {
            Some(0)
        } else {
            digits(whole)
        };
        let fraction_value = digits(fraction.get(..fraction.len().min(18)).unwrap_or_default());
        let (Some(whole), Some(fraction_value)) = (whole, fraction_value) else {
            return Err(invalid());
        };
        let fraction_len = u32::try_from(fraction.len().min(18)).unwrap_or(0);
        total = whole
            .checked_mul(scale)
            .and_then(|nanos| nanos.checked_add(fraction_value * scale / 10_u128.pow(fraction_len)))
            .and_then(|nanos| total.checked_add(nanos))
            .ok_or_else(invalid)?;
        rest = next;
    }
    let limit = u128::from(i64::MAX.unsigned_abs()) + u128::from(negative);
    if total > limit {
        return Err(invalid());
    }
    Ok(if negative {
        i64::try_from(-i128::try_from(total).map_err(|_| invalid())?).map_err(|_| invalid())?
    } else {
        i64::try_from(total).map_err(|_| invalid())?
    })
}

#[cfg(test)]
mod tests {
    use super::{format_nanos, parse_duration, quote};

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("0"), Ok(0));
        assert_eq!(parse_duration("90s"), Ok(90_000_000_000));
        assert_eq!(parse_duration("1h30m"), Ok(5_400_000_000_000));
        assert_eq!(parse_duration("-1.5h"), Ok(-5_400_000_000_000));
        assert_eq!(parse_duration("300ms"), Ok(300_000_000));
        assert_eq!(parse_duration(".5s"), Ok(500_000_000));
        for bad in ["", "-", "10", "10x", "s", "1..5s", "1s2", "99999999999999h"] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse_duration("10x").unwrap_err(),
            "time: invalid duration \"10x\""
        );
    }

    #[test]
    fn formats() {
        assert_eq!(quote("a\"b"), r#""a\"b""#);
        assert_eq!(format_nanos(-5), "0ns");
        assert_eq!(format_nanos(60_000_000_000), "60s");
    }
}
