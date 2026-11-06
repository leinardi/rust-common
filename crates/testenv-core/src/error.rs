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

//! The harness's one error type: a message that carries its context the way Go's wrapped errors
//! do (`sweep: remove container 1a2b3c4d5e6f: ...`).
//!
//! Extracted from swarm-device-access/testenv/src/error.rs at 8b4ab3a3e417 (`join`), with swarm-scheduler-exporter/testenv/src/error.rs at c9e79afa8810,
//! which inlined the same join.

use std::fmt;

/// A harness failure, with the chain of what was being done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    /// An error with this message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// This error with `context` in front, as Go's `fmt.Errorf("context: %w", err)`.
    #[must_use]
    pub fn context(self, context: impl fmt::Display) -> Self {
        Self(format!("{context}: {}", self.0))
    }

    /// Several errors as one, one message per line, as `errors.Join` in Go; `None` when there are
    /// none.
    #[must_use]
    pub fn join(errors: Vec<Self>) -> Option<Self> {
        if errors.is_empty() {
            return None;
        }
        let lines: Vec<String> = errors.into_iter().map(|error| error.0).collect();
        Some(Self(lines.join("\n")))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::Error;

    /// Go's `fmt.Errorf("%s: %w")` chain, and `errors.Join` of none, one and several.
    #[test]
    fn context_and_join_read_as_go() {
        let error = Error::new("refused")
            .context("remove container c1")
            .context("sweep");
        assert_eq!(error.to_string(), "sweep: remove container c1: refused");
        assert_eq!(Error::join(Vec::new()), None);
        assert_eq!(
            Error::join(vec![Error::new("a")]).map(|error| error.to_string()),
            Some("a".to_owned())
        );
        assert_eq!(
            Error::join(vec![Error::new("a"), Error::new("b")]).map(|error| error.to_string()),
            Some("a\nb".to_owned())
        );
    }
}
