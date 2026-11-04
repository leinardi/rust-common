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

//! A problem a check found: where, and what is wrong. Every check of this crate returns a list of
//! them, empty when the repository passes, and never panics on what it reads: a file it cannot
//! read or parse is a problem too. A meta-test asserts the list is empty, and its negative
//! control asserts what a planted violation produces.

use std::fmt;

/// One problem: where it is (a file, `file:line`, or nothing for a repository-wide rule) and what
/// is wrong.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Problem {
    /// Where: a slash-separated path relative to the root, `path:line`, or empty.
    pub location: String,
    /// What is wrong.
    pub message: String,
}

impl Problem {
    /// A problem with no location.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            location: String::new(),
            message: message.into(),
        }
    }

    /// A problem at `location`.
    #[must_use]
    pub fn at(location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            location: location.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    /// `<location>: <message>`, or the message alone without a location.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.location.is_empty() {
            formatter.write_str(&self.message)
        } else {
            write!(formatter, "{}: {}", self.location, self.message)
        }
    }
}

/// The problems, one per line, as an assertion message.
#[must_use]
pub fn report(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The problems as their display strings, for an exact comparison in a negative control.
#[must_use]
pub fn strings(problems: &[Problem]) -> Vec<String> {
    problems.iter().map(ToString::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::{Problem, report, strings};

    #[test]
    fn a_problem_reads_with_its_location() {
        let problems = [
            Problem::at("a.md:4", "link \"x\": x does not exist"),
            Problem::new("ci-result does not need the rust job"),
        ];
        assert_eq!(
            strings(&problems),
            [
                "a.md:4: link \"x\": x does not exist",
                "ci-result does not need the rust job"
            ]
        );
        assert_eq!(
            report(&problems),
            "a.md:4: link \"x\": x does not exist\nci-result does not need the rust job"
        );
        assert_eq!(report(&[]), "");
    }
}
