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

//! No gate passes on nothing: the files that define gates (the Makefile and its fragments, the
//! pre-commit configuration, every YAML file under `.github`, and a project's scripts) carry none
//! of the flags that turn a gate with nothing to check, or a failing one, into a pass.
//!
//! The flags come in sets: the core set every project uses (`|| true`, make's `-` prefix and
//! `.IGNORE` and `--ignore-errors`, `continue-on-error`, `if: false`); the Rust set (a test run
//! that only compiles, capped or allowed lints, tests skipped by name, a member excluded from a
//! workspace-wide command, and a clippy run without `-D warnings`); and the JavaScript set
//! (`--passWithNoTests`, `--no-error-on-unmatched-pattern`). A reviewed exception is an exact
//! fragment in one file, removed from the line before the flags are matched, so a second
//! `|| true` on an allowed line is still caught; an exception that matches nothing is a problem,
//! so it cannot outlive the code it excuses.
//!
//! Extracted from swarm-device-access/meta/tests/vacuous_flags.rs at 8b4ab3a3e417 (the core and
//! Rust sets, the clippy rule, the fragment exceptions and the line numbers) and
//! `bitgauge/meta/tests/vacuous_flags.rs` at 600fc3905200 (the JavaScript set).

use std::sync::LazyLock;

use regex::Regex;

use crate::{Problem, regex};

/// A flag that turns a gate with nothing to check, or a failing one, into a pass.
#[derive(Clone, Debug)]
pub struct Flag {
    /// The name a finding reports.
    pub name: &'static str,
    /// The pattern, matched per line.
    pub pattern: Regex,
}

fn flags(table: &[(&'static str, &str)]) -> Vec<Flag> {
    table
        .iter()
        .map(|(name, pattern)| Flag {
            name,
            pattern: regex(pattern),
        })
        .collect()
}

static CORE: LazyLock<Vec<Flag>> = LazyLock::new(|| {
    flags(&[
        ("|| true", r"\|\|\s*(true\b|:(\s|;|$)|exit\s+0\b)"),
        ("make's - recipe prefix", r"^\t[@+]*-"),
        (".IGNORE", r"^\.IGNORE\b"),
        (
            "make --ignore-errors",
            r"\$\(MAKE\).*\s(-[a-zA-Z]*i[a-zA-Z]*|--ignore-errors)\b",
        ),
        ("continue-on-error", r"continue-on-error"),
        (
            "if: false",
            r#"\bif:\s*(["']?false["']?\s*$|["']?\$\{\{\s*false\s*\}\}["']?)"#,
        ),
    ])
});

/// A cargo call is cargo itself or make's `$(CARGO)`.
static RUST: LazyLock<Vec<Flag>> = LazyLock::new(|| {
    flags(&[
        (
            "cargo test --no-run",
            r"(?i)\bcargo\)?\s.*\btest\b.*\s--no-run\b",
        ),
        ("--cap-lints", r"--cap-lints\b"),
        ("warnings allowed", r"(\s-A\s*|--allow[ =])warnings\b"),
        ("cargo --skip", r"(?i)\bcargo\)?\s.*\s--skip\b"),
        ("cargo --exclude", r"(?i)\bcargo\)?\s.*\s--exclude\b"),
    ])
});

static JAVASCRIPT: LazyLock<Vec<Flag>> = LazyLock::new(|| {
    flags(&[
        ("--passWithNoTests", r"--pass-?[Ww]ith-?[Nn]o-?[Tt]ests"),
        (
            "--no-error-on-unmatched-pattern",
            r"--no-error-on-unmatched-pattern",
        ),
    ])
});

/// A clippy run in a gate, which must turn warnings into errors.
static CLIPPY_CALL: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\bcargo\)?\s+(\+\S+\s+)?clippy\b"));
/// A YAML `name:` line: a hook's or a step's label, not a command it runs.
static YAML_NAME: LazyLock<Regex> = LazyLock::new(|| regex(r"^\s*(-\s+)?name:\s"));
/// The `-D warnings` a clippy gate needs.
static CLIPPY_DENIES_WARNINGS: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\s(-D\s*|--deny[ =])warnings\b"));

/// The flags a project's gates may not carry.
#[derive(Clone, Debug)]
pub struct VacuousFlags {
    flags: Vec<Flag>,
    clippy_denies_warnings: bool,
}

impl VacuousFlags {
    /// The core set alone.
    #[must_use]
    pub fn core() -> Self {
        Self {
            flags: CORE.clone(),
            clippy_denies_warnings: false,
        }
    }

    /// The set plus the Rust flags and the rule that a clippy run denies warnings.
    #[must_use]
    pub fn with_rust(mut self) -> Self {
        self.flags.extend(RUST.iter().cloned());
        self.clippy_denies_warnings = true;
        self
    }

    /// The set plus the JavaScript flags.
    #[must_use]
    pub fn with_javascript(mut self) -> Self {
        self.flags.extend(JAVASCRIPT.iter().cloned());
        self
    }

    /// The flags, in the order they are matched.
    #[must_use]
    pub fn flags(&self) -> &[Flag] {
        &self.flags
    }

    /// The flags on `content`'s lines (the text of `file`), after removing the fragments
    /// `allowed` excuses in `file`; `used` (as long as `allowed`) records which exceptions
    /// matched.
    #[must_use]
    pub fn hits(
        &self,
        file: &str,
        content: &str,
        allowed: &[AllowedFragment<'_>],
        used: &mut [bool],
    ) -> Vec<Hit> {
        let mut hits = Vec::new();
        for (index, original) in content.split('\n').enumerate() {
            let mut line = original.to_owned();
            for (allowed_index, entry) in allowed.iter().enumerate() {
                if entry.file == file && line.contains(entry.fragment) {
                    line = line.replace(entry.fragment, "");
                    if let Some(flag) = used.get_mut(allowed_index) {
                        *flag = true;
                    }
                }
            }
            if self.clippy_denies_warnings
                && CLIPPY_CALL.is_match(&line)
                && !CLIPPY_DENIES_WARNINGS.is_match(&line)
                && !YAML_NAME.is_match(&line)
            {
                hits.push(Hit {
                    line: index + 1,
                    flag: "cargo clippy without -D warnings",
                    text: original.trim().to_owned(),
                });
            }
            for flag in &self.flags {
                if flag.pattern.is_match(&line) {
                    hits.push(Hit {
                        line: index + 1,
                        flag: flag.name,
                        text: original.trim().to_owned(),
                    });
                }
            }
        }
        hits
    }

    /// Every flag in `files` (path and text) outside the `allowed` fragments, and every exception
    /// that matched no line.
    #[must_use]
    pub fn problems(
        &self,
        files: &[(String, String)],
        allowed: &[AllowedFragment<'_>],
    ) -> Vec<Problem> {
        let mut used = vec![false; allowed.len()];
        let mut problems = Vec::new();
        for (file, content) in files {
            for hit in self.hits(file, content, allowed, &mut used) {
                problems.push(Problem::at(
                    format!("{file}:{}", hit.line),
                    format!("{}: {}", hit.flag, hit.text),
                ));
            }
        }
        for (entry, used) in allowed.iter().zip(used) {
            if !used {
                problems.push(Problem::at(
                    entry.file,
                    format!(
                        "the exception {:?} ({}) matches nothing; remove it",
                        entry.fragment, entry.reason
                    ),
                ));
            }
        }
        problems
    }
}

/// One exact piece of a gate, in one file, whose flag is not a gate passing on nothing; the reason
/// says why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllowedFragment<'allowed> {
    /// The file, relative to the root.
    pub file: &'allowed str,
    /// The exact text excused.
    pub fragment: &'allowed str,
    /// Why it is not a gate passing on nothing.
    pub reason: &'allowed str,
}

/// One flag found on one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The line, counted from 1.
    pub line: usize,
    /// The flag's name.
    pub flag: &'static str,
    /// The line, trimmed.
    pub text: String,
}

#[cfg(test)]
mod tests;
