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

//! Go's `strconv.ParseBool`, as a Go program reads a boolean flag, label or setting: exactly its
//! twelve spellings, and its error text, so a message that quotes it reads as Go's.
//!
//! Extracted from swarm-device-access/src/go_compat/parse_bool.rs at 8b4ab3a3e417.

use std::fmt;

use super::quote;

/// The error of `strconv.ParseBool`: `strconv.ParseBool: parsing "<value>": invalid syntax`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseBoolError {
    value: String,
}

impl fmt::Display for ParseBoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "strconv.ParseBool: parsing {}: invalid syntax",
            quote::quote(&self.value)
        )
    }
}

impl std::error::Error for ParseBoolError {}

/// `strconv.ParseBool`: `1`, `t`, `T`, `TRUE`, `true` and `True` are true; `0`, `f`, `F`,
/// `FALSE`, `false` and `False` are false.
///
/// # Errors
///
/// Any other value, surrounding space and the empty string included.
pub fn parse_bool(value: &str) -> Result<bool, ParseBoolError> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        _ => Err(ParseBoolError {
            value: value.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exactly_the_go_spellings() {
        for value in ["1", "t", "T", "TRUE", "true", "True"] {
            assert_eq!(parse_bool(value), Ok(true), "{value}");
        }
        for value in ["0", "f", "F", "FALSE", "false", "False"] {
            assert_eq!(parse_bool(value), Ok(false), "{value}");
        }
        for value in ["", "yes", "on", " true", "tRUE", "2"] {
            assert!(parse_bool(value).is_err(), "{value}");
        }
        assert_eq!(
            parse_bool("y\"es").unwrap_err().to_string(),
            r#"strconv.ParseBool: parsing "y\"es": invalid syntax"#
        );
    }
}
