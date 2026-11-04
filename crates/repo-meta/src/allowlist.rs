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

//! A lint lifted only where an allowlist says: the rule is about the lint's name, so in the
//! scanned Rust files every mention of an allowlisted lint must be one of the accepted forms, in
//! an allowlisted place, and any other mention (an `allow`, a `cfg_attr`, an expect elsewhere,
//! even a comment) is refused, so no attribute syntax this scan does not model can slip past it.
//! A lint group that contains a boundary lint may not be named at all: lifting the group lifts
//! the lint with it.
//!
//! Two forms, by exemption:
//!
//! - [`Exemption::FileLevel`]: one file-level `#![expect(<lint>, reason = "...")]` in the file,
//!   and no other mention there (swarm-device-access's `unsafe_code` in its raw `bpf(2)` module).
//! - [`Exemption::Item`]: an item-level `#[expect(<lint>, reason = "...")]` line on the named item
//!   (a function, a type, or the type an `impl` is for), at most once (bitgauge's clippy bans on
//!   its healthcheck's connect and its test dialers).
//!
//! An attribute that rustfmt wrapped over several lines is joined first, so it reads as it would
//! unwrapped; a bracket inside a reason does not join the rest of the file into it. Each lint is
//! spelled as an attribute names it (`unsafe_code`, `clippy::disallowed_methods`) and found by its
//! last segment as a whole word. A test that scans its own crate spells the names in pieces
//! (`concat!`) so it does not mention them itself.
//!
//! Extracted from swarm-device-access/meta/tests/unsafe_allowlist.rs at 8b4ab3a3e417 (the
//! file-level form and the rule that any mention counts) and `bitgauge/meta/tests/lint_exemptions.rs`
//! at 600fc3905200 (the item-level form, the logical lines, the groups and the repeat count).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::{Problem, pattern, regex};

/// One place a lint may be lifted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Exemption<'allowlist> {
    /// One file-level `#![expect(<lint>, reason = "...")]` in `file`, and no other mention there.
    FileLevel {
        /// The file, relative to the root.
        file: &'allowlist str,
    },
    /// An item-level `#[expect(<lint>, reason = "...")]` line on `item` in `file`, at most once.
    Item {
        /// The file, relative to the root.
        file: &'allowlist str,
        /// The item's name.
        item: &'allowlist str,
    },
}

/// A lint and where it may be lifted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LintAllowlist<'allowlist> {
    /// The lint as an attribute names it: `unsafe_code`, `clippy::disallowed_methods`.
    pub lint: &'allowlist str,
    /// Where it may be lifted; empty for nowhere.
    pub exemptions: &'allowlist [Exemption<'allowlist>],
}

/// The name an item line declares: a function, a type, or the type an `impl` block is for.
static ITEM_NAME: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe)\s+)*(?:fn|struct|enum|type|static|const)\s+([A-Za-z_][A-Za-z0-9_]*)|^\s*impl(?:<[^>]*>)?\s+(?:[A-Za-z_][A-Za-z0-9_:<>]*\s+for\s+)?([A-Za-z_][A-Za-z0-9_]*)",
    )
});

struct Compiled<'rules> {
    lint: &'rules str,
    word: Regex,
    file_level: Regex,
    item: Regex,
    exemptions: &'rules [Exemption<'rules>],
}

/// What is wrong with how `files` (each path and its text) name the allowlisted `lints` and the
/// `groups` (such as `clippy::style` and `clippy::all` for the clippy bans): every mention of a
/// group, every mention of a lint outside an accepted form in an allowlisted place, every
/// exemption used more than once, and every exemption that matches nothing, so it cannot outlive
/// the code it excuses. No file or no lint is a problem.
#[must_use]
pub fn allowlist_problems(
    files: &BTreeMap<String, String>,
    lints: &[LintAllowlist<'_>],
    groups: &[&str],
) -> Vec<Problem> {
    if files.is_empty() {
        return vec![Problem::new(
            "no file to scan; the allowlist check would pass on nothing",
        )];
    }
    if lints.is_empty() {
        return vec![Problem::new(
            "no allowlisted lint; the allowlist check would pass on nothing",
        )];
    }
    let compiled: Vec<Compiled<'_>> = lints
        .iter()
        .map(|allowlist| {
            let name = allowlist.lint.rsplit("::").next().unwrap_or(allowlist.lint);
            let spelled = regex::escape(allowlist.lint);
            Compiled {
                lint: allowlist.lint,
                word: pattern(&format!(r"\b{}\b", regex::escape(name))),
                file_level: pattern(&format!(
                    r#"^\s*#!\[expect\(\s*{spelled}\s*,\s*reason\s*=\s*"[^"]+"\s*\)\]\s*$"#
                )),
                item: pattern(&format!(
                    r#"^\s*#\[expect\(\s*{spelled}\s*,\s*reason\s*=\s*"[^"]+"\s*\)\]\s*$"#
                )),
                exemptions: allowlist.exemptions,
            }
        })
        .collect();
    let group_word = (!groups.is_empty()).then(|| {
        pattern(&format!(
            r"(?:^|[^A-Za-z0-9_:])(?:{})\b",
            groups
                .iter()
                .map(|group| regex::escape(group))
                .collect::<Vec<_>>()
                .join("|")
        ))
    });

    let mut problems = Vec::new();
    let mut used: BTreeMap<(&str, Exemption<'_>), usize> = BTreeMap::new();
    for (path, text) in files {
        let logical = logical_lines(text);
        let texts: Vec<&str> = logical.iter().map(|(_, text)| text.as_str()).collect();
        for (index, line) in texts.iter().enumerate() {
            let at = format!("{path}:{}", logical[index].0);
            if group_word
                .as_ref()
                .is_some_and(|group| group.is_match(line))
            {
                problems.push(Problem::at(
                    at,
                    format!(
                        "names a lint group that contains an allowlisted lint ({groups:?}); \
                         lifting it lifts the lint everywhere"
                    ),
                ));
                continue;
            }
            for rule in &compiled {
                let mentions = rule.word.find_iter(line).count();
                if mentions == 0 {
                    continue;
                }
                let exemption = (mentions == 1)
                    .then(|| accepted(rule, path, line, &texts, index))
                    .flatten();
                match exemption {
                    Some(exemption) => *used.entry((rule.lint, exemption)).or_default() += 1,
                    None => problems.push(Problem::at(
                        at.clone(),
                        format!(
                            "names {}, which may be lifted only by {}",
                            rule.lint,
                            describe(rule.exemptions)
                        ),
                    )),
                }
            }
        }
    }
    problems.extend(exemption_count_problems(lints, &used));
    problems
}

/// Every exemption of `lints` that `used` (each exemption with the times it lifted its lint)
/// never saw, and every one used more than once.
fn exemption_count_problems(
    lints: &[LintAllowlist<'_>],
    used: &BTreeMap<(&str, Exemption<'_>), usize>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    for allowlist in lints {
        for exemption in allowlist.exemptions {
            if !used.contains_key(&(allowlist.lint, *exemption)) {
                let (file, place) = match exemption {
                    Exemption::FileLevel { file } => (*file, "file-level".to_owned()),
                    Exemption::Item { file, item } => (*file, format!("on {item}")),
                };
                problems.push(Problem::at(
                    file,
                    format!(
                        "the exemption for {} {place} matches nothing; remove it",
                        allowlist.lint
                    ),
                ));
            }
        }
    }
    for ((lint, exemption), count) in used {
        if *count > 1 {
            let (file, place) = match exemption {
                Exemption::FileLevel { file } => (file, "file-level".to_owned()),
                Exemption::Item { file, item } => (file, format!("on {item}")),
            };
            problems.push(Problem::at(
                *file,
                format!("lifts {lint} {place} {count} times; at most once"),
            ));
        }
    }
    problems
}

/// The exemption `line` (the one mention of the rule's lint) uses in `path`, if it is one.
fn accepted<'rules>(
    rule: &Compiled<'rules>,
    path: &str,
    line: &str,
    lines: &[&str],
    index: usize,
) -> Option<Exemption<'rules>> {
    if rule.file_level.is_match(line) {
        return rule
            .exemptions
            .iter()
            .copied()
            .find(|exemption| matches!(exemption, Exemption::FileLevel { file } if *file == path));
    }
    if rule.item.is_match(line) {
        let item = item_after(lines, index)?;
        return rule.exemptions.iter().copied().find(|exemption| {
            matches!(exemption, Exemption::Item { file, item: allowed } if *file == path && *allowed == item)
        });
    }
    None
}

fn describe(exemptions: &[Exemption<'_>]) -> String {
    if exemptions.is_empty() {
        return "no file".to_owned();
    }
    exemptions
        .iter()
        .map(|exemption| match exemption {
            Exemption::FileLevel { file } => {
                format!("{file}'s one file-level #![expect(..., reason = \"...\")]")
            }
            Exemption::Item { file, item } => {
                format!("an item-level #[expect(..., reason = \"...\")] line on {item} in {file}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The text as logical lines, each with its first line's number: an attribute that rustfmt
/// wrapped over several lines is joined into one.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut joined: Vec<(usize, String)> = Vec::new();
    let mut open: Option<(usize, String, i32)> = None;
    for (index, line) in text.lines().enumerate() {
        if let Some((number, mut attribute, depth)) = open.take() {
            attribute.push(' ');
            attribute.push_str(line.trim());
            let depth = depth + bracket_depth(line);
            if depth > 0 {
                open = Some((number, attribute, depth));
            } else {
                joined.push((number, attribute));
            }
            continue;
        }
        let trimmed = line.trim_start();
        let depth = bracket_depth(line);
        if (trimmed.starts_with("#[") || trimmed.starts_with("#![")) && depth > 0 {
            open = Some((index + 1, line.trim_end().to_owned(), depth));
        } else {
            joined.push((index + 1, line.to_owned()));
        }
    }
    if let Some((number, attribute, _)) = open {
        joined.push((number, attribute));
    }
    joined
}

/// How many more square brackets and parentheses a line opens than it closes, outside string
/// literals.
fn bracket_depth(line: &str) -> i32 {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for character in line.chars() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// The item the attribute on line `index` applies to: the first following line that is neither
/// an attribute, a comment nor blank; `None` when there is none or it declares no name this reader
/// knows.
fn item_after<'lines>(lines: &[&'lines str], index: usize) -> Option<&'lines str> {
    let line = lines.iter().skip(index + 1).find(|line| {
        let trimmed = line.trim_start();
        !(trimmed.is_empty() || trimmed.starts_with("#[") || trimmed.starts_with("//"))
    })?;
    ITEM_NAME
        .captures(line)
        .and_then(|found| found.get(1).or_else(|| found.get(2)))
        .map(|name| name.as_str())
}

#[cfg(test)]
mod tests;
