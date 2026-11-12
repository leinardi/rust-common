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

//! How a value is written in a problem's text: a quoted text, a float and a duration, in plain
//! Rust formatting (`{:?}`, `f64`'s `Display`, jiff's friendly duration).
//!
//! Extracted from bitgauge/src/config/text.rs at 9be3b0d20765; the author relicenses it from
//! GPL-3.0-or-later to MIT OR Apache-2.0 for this repository.

use jiff::SignedDuration;

/// `text` quoted and escaped as Rust's `Debug` does (`"a\"b"`, `"\u{7f}"`).
#[must_use]
pub fn quote(text: &str) -> String {
    format!("{text:?}")
}

/// `value` as `f64`'s `Display` writes it: `0.5`, `1`, `100`, never an exponent.
#[must_use]
pub fn float(value: f64) -> String {
    value.to_string()
}

/// `value` in jiff's friendly duration form: `1m 30s`, `500ms`, `2h`, `1s ago`.
#[must_use]
pub fn duration(value: SignedDuration) -> String {
    format!("{value:#}")
}
