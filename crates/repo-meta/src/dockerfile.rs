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

//! Reads a Dockerfile closely enough to assert an image's invariants: the syntax directive, the
//! stages, their bases and the instructions inside them, and the base pins. A file this reader
//! cannot take apart is refused ([`Malformed`]) rather than guessed at.
//!
//! The pin rules: a dhi.io base is pinned by an explicit tag alone, so a build takes the fixes
//! dhi.io republishes under it; any other base, and the `BuildKit` frontend, by tag and digest
//! ([`base_pinned`]). A variable anywhere in a reference makes it unpinned: a reader cannot know
//! its value.
//!
//! Extracted from swarm-scheduler-exporter/meta/src/dockerfile.rs at c9e79afa8810 (the reader,
//! [`Malformed`], [`Stage::joined`], and [`tag_pinned`]'s guard that the reference has a tag) and
//! bitgauge/meta/tests/dockerfiles/reader.rs at 600fc3905200 ([`from_dhi`], [`base_pinned`], the
//! tag split that reads a colon before the last slash as a registry port, and `latest` refused in
//! any case).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

use crate::{Problem, regex};

/// One Dockerfile instruction with its continuation lines joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    /// The instruction, upper-cased: FROM, RUN, COPY, ...
    pub keyword: String,
    /// Everything after the keyword, with continuations joined and whitespace runs collapsed.
    pub args: String,
}

/// One FROM and the instructions up to the next.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stage {
    /// The AS alias, empty for an unnamed stage.
    pub name: String,
    /// The `--platform` value, empty when the stage has none.
    pub platform: String,
    /// The base reference exactly as written.
    pub image: String,
    /// The instructions, the FROM itself excluded.
    pub instructions: Vec<Instruction>,
}

impl Stage {
    /// The stage's instructions, one `KEYWORD args` per line.
    #[must_use]
    pub fn joined(&self) -> String {
        self.instructions
            .iter()
            .map(|instruction| format!("{} {}", instruction.keyword, instruction.args))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A Dockerfile this reader cannot take apart; the checks refuse it rather than guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Malformed(pub String);

impl fmt::Display for Malformed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "malformed Dockerfile: {}", self.0)
    }
}

impl std::error::Error for Malformed {}

/// A literal registry reference with a tag and a sha256 digest.
static DIGEST_PINNED: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^[a-z0-9.\-/:]+:[A-Za-z0-9._\-]+@sha256:[0-9a-f]{64}$"));

/// A literal registry reference with a tag and no digest.
static TAG_PINNED: LazyLock<Regex> = LazyLock::new(|| regex(r"^[a-z0-9.\-/:]+:[A-Za-z0-9._\-]+$"));

/// One `# key=value` line of the block `BuildKit` reads before anything else.
static PARSER_DIRECTIVE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^#\s*([A-Za-z]+)\s*=\s*(\S+)\s*$"));

/// One `key=value` pair of a LABEL, the value quoted or bare.
static LABEL_PAIR: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"([A-Za-z0-9._\-]+)=("(?:[^"\\]|\\.)*"|\S+)"#));

/// The registry whose bases are pinned by tag, not by digest.
pub const DHI_REGISTRY: &str = "dhi.io/";

/// The value of the file's syntax parser directive. `BuildKit` reads directives only from the
/// file's leading lines, and stops at the first line that is not one (a comment, a blank line or
/// an instruction), so a `# syntax=` line anywhere later is an ordinary comment: this reader stops
/// at the same place, and reports the directive missing rather than find one `BuildKit` ignores.
#[must_use]
pub fn syntax(content: &str) -> Option<String> {
    for line in content.split('\n') {
        let captures = PARSER_DIRECTIVE.captures(line.trim())?;
        if captures[1].eq_ignore_ascii_case("syntax") {
            return Some(captures[2].to_owned());
        }
    }
    None
}

/// Splits a Dockerfile into instructions, dropping comments and blank lines and joining backslash
/// continuations, and groups them into stages.
///
/// # Errors
///
/// An instruction other than ARG before the first FROM, a FROM this reader cannot read, or a file
/// that ends inside a continuation.
pub fn parse(content: &str) -> Result<Vec<Stage>, Malformed> {
    let mut stages: Vec<Stage> = Vec::new();
    let mut pending = String::new();
    for line in content.split('\n') {
        let trimmed = line.trim();
        // A comment line is dropped even inside a continuation, as BuildKit does.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(body) = trimmed.strip_suffix('\\') {
            pending.push_str(body);
            pending.push(' ');
            continue;
        }
        pending.push_str(trimmed);
        let instruction = pending.split_whitespace().collect::<Vec<_>>().join(" ");
        pending.clear();
        let (keyword, args) = instruction
            .split_once(' ')
            .unwrap_or((instruction.as_str(), ""));
        let keyword = keyword.to_ascii_uppercase();
        if keyword == "FROM" {
            stages.push(parse_from(args)?);
            continue;
        }
        let Some(last) = stages.last_mut() else {
            if keyword == "ARG" {
                continue;
            }
            return Err(Malformed(format!("{keyword} before the first FROM")));
        };
        last.instructions.push(Instruction {
            keyword,
            args: args.to_owned(),
        });
    }
    if !pending.trim().is_empty() {
        return Err(Malformed("the file ends inside a continuation".to_owned()));
    }
    Ok(stages)
}

fn parse_from(args: &str) -> Result<Stage, Malformed> {
    let mut stage = Stage::default();
    let mut fields: Vec<&str> = args.split_whitespace().collect();
    while let Some(flag) = fields.first().filter(|field| field.starts_with("--")) {
        if let Some(platform) = flag.strip_prefix("--platform=") {
            platform.clone_into(&mut stage.platform);
        }
        fields.remove(0);
    }
    match fields.as_slice() {
        [image] => (*image).clone_into(&mut stage.image),
        [image, alias, name] if alias.eq_ignore_ascii_case("AS") => {
            (*image).clone_into(&mut stage.image);
            (*name).clone_into(&mut stage.name);
        }
        _ => return Err(Malformed(format!("FROM {args}"))),
    }
    Ok(stage)
}

/// The one stage of `stages` named `name`.
///
/// # Errors
///
/// No stage, or more than one, has the name: a problem at `file`.
pub fn stage_named<'stages>(
    file: &str,
    stages: &'stages [Stage],
    name: &str,
) -> Result<&'stages Stage, Problem> {
    let found: Vec<&Stage> = stages.iter().filter(|stage| stage.name == name).collect();
    match found.as_slice() {
        [stage] => Ok(stage),
        found => Err(Problem::at(
            file,
            format!("has {} stages named {name}, want one", found.len()),
        )),
    }
}

/// Whether a base reference is literal and pinned by a tag other than `latest` (in any case),
/// without a digest.
#[must_use]
pub fn tag_pinned(image: &str) -> bool {
    TAG_PINNED.is_match(image)
        && repository(image) != image
        && !tag(image).eq_ignore_ascii_case("latest")
}

/// Whether a base reference is literal and pinned by tag and digest.
#[must_use]
pub fn digest_pinned(image: &str) -> bool {
    DIGEST_PINNED.is_match(image)
}

/// Whether a base reference comes from dhi.io.
#[must_use]
pub fn from_dhi(image: &str) -> bool {
    image.starts_with(DHI_REGISTRY)
}

/// Whether a base reference is pinned the way its registry requires: a dhi.io base by an explicit
/// tag and no digest, any other by tag and digest.
#[must_use]
pub fn base_pinned(image: &str) -> bool {
    if from_dhi(image) {
        tag_pinned(image)
    } else {
        digest_pinned(image)
    }
}

/// The reference without its digest, split at the tag's colon when it has one. A colon before the
/// last slash is a registry port, not a tag.
fn split_tag(image: &str) -> (&str, Option<&str>) {
    let name = image.split_once('@').map_or(image, |(name, _)| name);
    let slash = name.rfind('/');
    match name.rfind(':') {
        Some(colon) if slash.is_none_or(|slash| colon > slash) => {
            (&name[..colon], Some(&name[colon + 1..]))
        }
        Some(_) | None => (name, None),
    }
}

/// The reference without its tag and digest: `dhi.io/rust` for
/// `dhi.io/rust:1.98.1-debian13-dev`, `registry:5000/app` for `registry:5000/app:1`.
#[must_use]
pub fn repository(image: &str) -> &str {
    split_tag(image).0
}

/// The tag of a reference, empty when it has none.
#[must_use]
pub fn tag(image: &str) -> &str {
    split_tag(image).1.unwrap_or("")
}

/// The LABEL key/value pairs of a stage, with the quotes around values removed.
#[must_use]
pub fn labels(stage: &Stage) -> BTreeMap<String, String> {
    stage
        .instructions
        .iter()
        .filter(|instruction| instruction.keyword == "LABEL")
        .flat_map(|instruction| {
            LABEL_PAIR
                .captures_iter(&instruction.args)
                .map(|captures| {
                    (
                        captures[1].to_owned(),
                        captures[2].trim_matches('"').to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests;
