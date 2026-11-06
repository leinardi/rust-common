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

//! For a project whose meta-tests fail without the tools they need: no meta-test looks a tool up
//! on `PATH` (a skip on a missing tool is a pass CI cannot turn into a failure), and none reads a
//! switch that turns such a skip into a failure. A project that gates its tool tests with
//! [`testenv_core::toolgate::Gate`] instead has the other policy, and does not run this check.
//!
//! Extracted from swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417
//! (`no_meta_test_skips_on_a_missing_tool`, with its switch) and
//! swarm-scheduler-exporter/meta/tests/toolchain.rs at c9e79afa8810 (the same lookups).

use crate::{Problem, pattern};

/// The files of `files` (path and text, the meta crate's sources) that look a tool up on `PATH`
/// (`split_paths(`, `var_os("PATH"`, `Command::new("which"`) or name one of `switches` (such as
/// `SDA_REQUIRE_TOOLS`) as a whole word; and no file is a problem.
#[must_use]
pub fn tool_lookup_problems(files: &[(String, String)], switches: &[&str]) -> Vec<Problem> {
    if files.is_empty() {
        return vec![Problem::new(
            "no meta-test source to scan; the check would pass on nothing",
        )];
    }
    let mut alternatives = vec![
        r"\bsplit_paths\(".to_owned(),
        r"\bvar_os\(\s*\x22PATH\x22".to_owned(),
        r"Command::new\(\s*\x22which\x22".to_owned(),
    ];
    alternatives.extend(
        switches
            .iter()
            .map(|switch| format!(r"\b{}\b", regex::escape(switch))),
    );
    let lookup = pattern(&alternatives.join("|"));
    files
        .iter()
        .filter(|(_, text)| lookup.is_match(text))
        .map(|(path, _)| {
            Problem::at(
                path,
                "looks a tool up on PATH or reads a skip switch; a meta-test needing a tool must \
                 fail without it",
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::tool_lookup_problems;

    /// The samples are split, so this file does not match its own check.
    #[test]
    fn each_lookup_is_recognised() {
        let switch = concat!("SDA_REQUIRE", "_TOOLS");
        for sample in [
            concat!("let found = env::split", "_paths(&paths);"),
            concat!("std::env::var", "_os(\x22PATH\x22)"),
            concat!("Command::new(\x22wh", "ich\x22)"),
            concat!("env::var(\x22SDA_REQUIRE", "_TOOLS\x22)"),
        ] {
            let files = [("meta/tests/x.rs".to_owned(), sample.to_owned())];
            assert_eq!(
                tool_lookup_problems(&files, &[switch]).len(),
                1,
                "{sample:?} was not recognised"
            );
        }
        let files = [(
            "meta/tests/x.rs".to_owned(),
            concat!("env::var(\x22SDA_REQUIRE", "_TOOLS_X\x22)").to_owned(),
        )];
        assert_eq!(tool_lookup_problems(&files, &[switch]), []);
        let plain = [("meta/tests/x.rs".to_owned(), "let x = 1;".to_owned())];
        assert_eq!(tool_lookup_problems(&plain, &[]), []);
    }

    /// No file is a problem, not a pass on nothing.
    #[test]
    fn no_file_is_a_problem() {
        assert_eq!(
            crate::strings(&tool_lookup_problems(&[], &[])),
            ["no meta-test source to scan; the check would pass on nothing"]
        );
    }
}
