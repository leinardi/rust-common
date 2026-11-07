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

//! The tooling pins: every version written in more than one place is one value everywhere it is
//! written, so a bump in one file cannot leave another building, linting or testing with the old
//! one.
//!
//! - [`one_value`]: the general rule, over [`PinSource`]s (a file and the pattern whose first
//!   group is the pin there) and values found elsewhere.
//! - The Rust release's sources are presets of [`PinSource`], for the places every project repeats
//!   it: [`PinSource::rust_channel`] and [`PinSource::rust_channel_line`] (`rust-toolchain.toml`),
//!   [`PinSource::rust_version`] and [`PinSource::rust_version_line`] (Cargo's `rust-version`),
//!   [`PinSource::rust_image_tag`] (the `dhi.io/rust` tag of a `FROM`) and
//!   [`PinSource::rust_version_arg`] (`ARG RUST_VERSION`). [`rust_release`] reads the exact
//!   release, and [`toolchain_targets`] the targets, from `rust-toolchain.toml`.
//! - [`make_value`]: the value make gives a variable, for a pin a Makefile holds.
//!
//! Any other pin (a tool's version, a setup action's input) is a [`PinSource`] with the project's
//! own pattern.
//!
//! Extracted from swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417,
//! swarm-scheduler-exporter/meta/tests/toolchain.rs at c9e79afa8810 (`expect_one_value`) and
//! bitgauge/meta/tests/toolchain.rs at 600fc3905200 (optional sources and extra values).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::{Problem, Repo, pattern, regex};

/// One place a pin is written: a file, its content (`None` when it cannot be read), and the
/// pattern whose first group is the pin there; every match counts. A required source that is
/// unreadable or writes no match is a problem; an optional one may be absent.
#[derive(Clone, Debug)]
pub struct PinSource {
    /// The file, relative to the root.
    pub file: String,
    /// Its content.
    pub content: Option<String>,
    /// The pattern; its first group is the pin.
    pub pattern: Regex,
    /// Whether the source may be absent or write nothing.
    pub optional: bool,
}

impl PinSource {
    /// A required source with the given content.
    ///
    /// # Panics
    ///
    /// `pattern` does not compile ([`crate::pattern`]).
    #[must_use]
    pub fn new(file: &str, content: &str, pattern: &str) -> Self {
        Self {
            file: file.to_owned(),
            content: Some(content.to_owned()),
            pattern: crate::pattern(pattern),
            optional: false,
        }
    }

    /// A required source read from `repo`; unreadable, it is a problem.
    ///
    /// # Panics
    ///
    /// `pattern` does not compile ([`crate::pattern`]).
    #[must_use]
    pub fn read(repo: &Repo, file: &str, pattern: &str) -> Self {
        Self {
            file: file.to_owned(),
            content: repo.try_read(file).ok(),
            pattern: crate::pattern(pattern),
            optional: false,
        }
    }

    /// The same source, allowed to be absent or to write nothing.
    #[must_use]
    pub fn optional(self) -> Self {
        Self {
            optional: true,
            ..self
        }
    }
}

/// One pin found, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinValue {
    /// The value.
    pub value: String,
    /// The file it is written in.
    pub file: String,
}

impl PinValue {
    /// `value`, found in `file`.
    #[must_use]
    pub fn new(value: &str, file: &str) -> Self {
        Self {
            value: value.to_owned(),
            file: file.to_owned(),
        }
    }
}

/// What is wrong unless every required source writes the pin and all the values found, `extra`
/// included, are one value: a problem per required source that is unreadable or writes nothing,
/// one listing the values when there are several, and one when there is neither a source nor a
/// value.
#[must_use]
pub fn one_value(what: &str, sources: &[PinSource], extra: &[PinValue]) -> Vec<Problem> {
    if sources.is_empty() && extra.is_empty() {
        return vec![Problem::new(format!(
            "{what}: no source and no value; the check would pass on nothing"
        ))];
    }
    let mut problems = Vec::new();
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for value in extra {
        found
            .entry(value.value.clone())
            .or_default()
            .push(value.file.clone());
    }
    for source in sources {
        let Some(content) = &source.content else {
            if !source.optional {
                problems.push(Problem::new(format!(
                    "{what}: {} cannot be read",
                    source.file
                )));
            }
            continue;
        };
        let mut matched = false;
        for captures in source.pattern.captures_iter(content) {
            matched = true;
            found
                .entry(captures[1].to_owned())
                .or_default()
                .push(source.file.clone());
        }
        if !matched && !source.optional {
            problems.push(Problem::new(format!(
                "{what}: {} does not write it ({})",
                source.file, source.pattern
            )));
        }
    }
    if found.is_empty() && problems.is_empty() {
        problems.push(Problem::new(format!(
            "{what}: no source writes it; the check would pass on nothing"
        )));
    }
    if found.len() > 1 {
        let values: Vec<String> = found
            .into_iter()
            .map(|(value, mut files)| {
                files.dedup();
                format!("{value} in {}", files.join(", "))
            })
            .collect();
        problems.push(Problem::new(format!(
            "{what} is not one value: {}",
            values.join("; ")
        )));
    }
    problems
}

/// `rust-toolchain.toml`'s channel.
const RUST_CHANNEL: &str = r#"(?m)^channel\s*=\s*"([^"]*)"\s*$"#;
/// `rust-toolchain.toml`'s channel, as its `X.Y` line.
const RUST_CHANNEL_LINE: &str = r#"(?m)^channel\s*=\s*"(\d+\.\d+)\.\d+"\s*$"#;
/// A Cargo `rust-version`.
const RUST_VERSION: &str = r#"(?m)^rust-version\s*=\s*"([^"]*)"\s*$"#;
/// A Cargo `rust-version`, as its `X.Y` line.
const RUST_VERSION_LINE: &str = r#"(?m)^rust-version\s*=\s*"(\d+\.\d+)(?:\.\d+)?"\s*$"#;
/// The Rust release of a `dhi.io/rust` base's tag, on a FROM line.
const RUST_IMAGE_TAG: &str = r"(?mi)^[ \t]*FROM[ \t]+(?:--\S+[ \t]+)*dhi\.io/rust:([0-9][\w.]*?)-";
/// A Dockerfile's `ARG RUST_VERSION` default.
const RUST_VERSION_ARG: &str = r"(?m)^ARG RUST_VERSION=(\S+)\s*$";

/// The sources of the Rust release, which every project repeats.
impl PinSource {
    /// The exact release (`1.98.1`) of `rust-toolchain.toml`'s channel, `file` with `content`.
    #[must_use]
    pub fn rust_channel(file: &str, content: &str) -> Self {
        Self::new(file, content, RUST_CHANNEL)
    }

    /// The `X.Y` line (`1.98`) of `rust-toolchain.toml`'s channel; a channel that is not a release
    /// writes no match, so it is a problem.
    #[must_use]
    pub fn rust_channel_line(file: &str, content: &str) -> Self {
        Self::new(file, content, RUST_CHANNEL_LINE)
    }

    /// Every `rust-version` of a Cargo manifest, as written.
    #[must_use]
    pub fn rust_version(file: &str, content: &str) -> Self {
        Self::new(file, content, RUST_VERSION)
    }

    /// Every `rust-version` of a Cargo manifest, as its `X.Y` line, whether it is written `1.98` or
    /// `1.98.1`.
    #[must_use]
    pub fn rust_version_line(file: &str, content: &str) -> Self {
        Self::new(file, content, RUST_VERSION_LINE)
    }

    /// The release in the tag of every `FROM dhi.io/rust:<release>-<variant>` of a Dockerfile.
    #[must_use]
    pub fn rust_image_tag(file: &str, content: &str) -> Self {
        Self::new(file, content, RUST_IMAGE_TAG)
    }

    /// The default of a Dockerfile's `ARG RUST_VERSION=<release>`.
    #[must_use]
    pub fn rust_version_arg(file: &str, content: &str) -> Self {
        Self::new(file, content, RUST_VERSION_ARG)
    }
}

/// A Rust release with its patch: 1.98.1, never 1.98 or stable.
static EXACT_RUST: LazyLock<Regex> = LazyLock::new(|| regex(r"^\d+\.\d+\.\d+$"));

/// The exact release `rust-toolchain.toml` (the text `toolchain`, at `file`) pins.
///
/// # Errors
///
/// It names no channel, or one that is not an exact `X.Y.Z` release (`stable`, `1.98`, a
/// nightly).
pub fn rust_release(file: &str, toolchain: &str) -> Result<String, Problem> {
    match regex(RUST_CHANNEL).captures(toolchain) {
        Some(channel) if EXACT_RUST.is_match(&channel[1]) => Ok(channel[1].to_owned()),
        Some(channel) => Err(Problem::at(
            file,
            format!("pins channel {:?}, not an exact X.Y.Z release", &channel[1]),
        )),
        None => Err(Problem::at(file, "names no channel")),
    }
}

/// The targets `rust-toolchain.toml` installs, empty when it names none or is not TOML.
#[must_use]
pub fn toolchain_targets(toolchain: &str) -> Vec<String> {
    toolchain
        .parse::<toml::Table>()
        .ok()
        .and_then(|table| {
            table
                .get("toolchain")
                .and_then(|section| section.get("targets"))
                .and_then(toml::Value::as_array)
                .map(|targets| {
                    targets
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// The value make gives `<variable>` in `text`, assignments read in order: `=`, `:=` and `::=`
/// set it, `?=` only when it is unset, `+=` appends with a space, and `!=` (a shell command's
/// output) is kept as `!=<command>`, which matches no pin; an `export` or `override` prefix is
/// read past. `None` when nothing assigns it.
#[must_use]
pub fn make_value(text: &str, variable: &str) -> Option<String> {
    let assignment = pattern(&format!(
        r"(?m)^(?:(?:export|override)[ \t]+)*{}[ \t]*(::|:|\?|\+|!)?=[ \t]*(.*?)[ \t]*$",
        regex::escape(variable)
    ));
    let mut value: Option<String> = None;
    for captures in assignment.captures_iter(text) {
        let assigned = captures[2].to_owned();
        value = match (captures.get(1).map(|flavour| flavour.as_str()), value) {
            (Some("?"), Some(current)) => Some(current),
            (Some("+"), Some(current)) => Some(format!("{current} {assigned}")),
            (Some("!"), _) => Some(format!("!={assigned}")),
            (_, _) => Some(assigned),
        };
    }
    value
}

#[cfg(test)]
mod tests;
