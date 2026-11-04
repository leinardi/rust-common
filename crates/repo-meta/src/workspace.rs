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

//! The workspace's lint set reaches every member: the root manifest declares
//! `[workspace.lints.rust]` and `[workspace.lints.clippy]`, every member is listed explicitly and
//! declares `[lints] workspace = true` (`[lints]` is not inherited, so a member that forgets it
//! builds and passes clippy with the default lints, silently), the root sets the levels a project
//! relies on, and nothing outside the manifest names a lint whose level it could change. Also the
//! member layout of a workspace of `crates/<name>` members.
//!
//! Extracted from swarm-device-access/meta/tests/workspace_lints.rs at 8b4ab3a3e417 (the checks as
//! functions of the manifests), `bitgauge/meta/tests/lint_inheritance.rs` at 600fc3905200 (a `.`
//! member, the expected member list, the levels the root must set, and the scan for overrides in
//! Cargo configuration and make), swarm-device-access/meta/tests/unsafe_allowlist.rs at
//! 8b4ab3a3e417 (a level read in either form) and rust-common's own meta crate (the layout).

use std::collections::BTreeSet;

use toml::{Table, Value};

use crate::{Problem, Repo};

fn parse(file: &str, text: &str) -> Result<Table, Problem> {
    text.parse::<Table>()
        .map_err(|error| Problem::at(file, format!("parse: {error}")))
}

fn read_manifest(repo: &Repo, file: &str) -> Result<Table, Problem> {
    parse(file, &repo.try_read(file)?)
}

/// The manifest of a member: `Cargo.toml` for `.`, `<member>/Cargo.toml` otherwise.
#[must_use]
pub fn member_manifest(member: &str) -> String {
    if member == "." {
        "Cargo.toml".to_owned()
    } else {
        format!("{member}/Cargo.toml")
    }
}

/// The `[workspace] members` of a root manifest, as written.
///
/// # Errors
///
/// It lists no members, or a member is not a string.
pub fn members(root: &Table) -> Result<Vec<String>, Problem> {
    let members = root
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(Value::as_array)
        .ok_or_else(|| Problem::new("the root Cargo.toml lists no [workspace] members"))?;
    if members.is_empty() {
        return Err(Problem::new("the workspace lists no members"));
    }
    members
        .iter()
        .map(|member| {
            member.as_str().map(str::to_owned).ok_or_else(|| {
                Problem::new(format!("workspace member {member} is not a path string"))
            })
        })
        .collect()
}

/// Checks the root manifest's `[workspace.lints]` and every member's opt-in, reading the
/// manifests under `repo`; with `expected_members`, the members must be exactly those, in that
/// order (bitgauge's, whose lint boundary is laid out per member).
#[must_use]
pub fn lint_opt_in(repo: &Repo, expected_members: Option<&[&str]>) -> Vec<Problem> {
    let root = match read_manifest(repo, "Cargo.toml") {
        Ok(root) => root,
        Err(problem) => return vec![problem],
    };
    let mut problems = root_lint_problems(&root);
    problems.extend(member_opt_in_problems(
        &root,
        |member| read_manifest(repo, &member_manifest(member)),
        expected_members,
    ));
    problems
}

/// What is wrong with a root manifest's `[workspace.lints]`: the table, and its `rust` and
/// `clippy` sections.
#[must_use]
pub fn root_lint_problems(root: &Table) -> Vec<Problem> {
    let Some(lints) = root
        .get("workspace")
        .and_then(|workspace| workspace.get("lints"))
        .and_then(Value::as_table)
    else {
        return vec![Problem::new(
            "the root Cargo.toml has no [workspace.lints] table",
        )];
    };
    ["rust", "clippy"]
        .into_iter()
        .filter(|section| !lints.contains_key(*section))
        .map(|section| Problem::new(format!("[workspace.lints.{section}] is missing")))
        .collect()
}

/// Every member that is a glob or does not opt into the workspace lints, `read_member` reading a
/// member's manifest by its member path, and with `expected_members` any difference from that
/// list.
pub fn member_opt_in_problems(
    root: &Table,
    read_member: impl Fn(&str) -> Result<Table, Problem>,
    expected_members: Option<&[&str]>,
) -> Vec<Problem> {
    let members = match members(root) {
        Ok(members) => members,
        Err(problem) => return vec![problem],
    };
    let mut problems = Vec::new();
    let mut listed = Vec::new();
    for member in &members {
        if member.contains('*') {
            problems.push(Problem::new(format!(
                "member {member:?} is a glob; list members explicitly so this check sees each one"
            )));
            continue;
        }
        listed.push(member.as_str());
        match read_member(member) {
            Ok(manifest) => {
                let opts_in = manifest
                    .get("lints")
                    .and_then(|lints| lints.get("workspace"))
                    .and_then(Value::as_bool);
                if opts_in != Some(true) {
                    problems.push(Problem::new(format!(
                        "{} does not declare [lints] workspace = true",
                        member_manifest(member)
                    )));
                }
            }
            Err(problem) => problems.push(problem),
        }
    }
    if let Some(expected) = expected_members
        && listed != expected
    {
        problems.push(Problem::new(format!(
            "the workspace members are {listed:?}; want exactly {expected:?}"
        )));
    }
    problems
}

/// A lint level the root manifest must set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LintLevel<'level> {
    /// The `[workspace.lints.<table>]` section: `rust` or `clippy`.
    pub table: &'level str,
    /// The lint, as the section names it: `unsafe_code`, `disallowed_methods`.
    pub lint: &'level str,
    /// The level: `deny`, `forbid`.
    pub level: &'level str,
}

/// The level a lint table sets for `lint`, in the `"deny"` or the `{ level = "deny" }` form.
#[must_use]
pub fn level<'table>(lints: &'table Table, lint: &str) -> Option<&'table str> {
    let value = lints.get(lint)?;
    value
        .as_str()
        .or_else(|| value.get("level").and_then(Value::as_str))
}

/// Every one of `required` the root manifest does not set to exactly its level: `warn` or
/// `allow` would let the lint through, and a `forbid` where a project lifts the lint in one
/// allowlisted place could not be lifted.
#[must_use]
pub fn lint_level_problems(root: &Table, required: &[LintLevel<'_>]) -> Vec<Problem> {
    required
        .iter()
        .filter_map(|want| {
            let set = root
                .get("workspace")
                .and_then(|workspace| workspace.get("lints"))
                .and_then(|lints| lints.get(want.table))
                .and_then(Value::as_table)
                .and_then(|lints| level(lints, want.lint));
            match set {
                Some(level) if level == want.level => None,
                Some(other) => Some(Problem::new(format!(
                    "[workspace.lints.{}] sets {} = {other:?}; it must be {:?}",
                    want.table, want.lint, want.level
                ))),
                None => Some(Problem::new(format!(
                    "[workspace.lints.{}] does not set {} = {:?}",
                    want.table, want.lint, want.level
                ))),
            }
        })
        .collect()
}

/// Every one of `files` (path and text: Cargo configuration, the Makefile, the make fragments)
/// that names one of `tokens`: a lint name, a group, `--cap-lints` or `CLIPPY_CONF_DIR`. Flags
/// from there are passed after the manifest's `[lints]`, so they win. The match is on the text,
/// so a comment counts too: the rule is about the name, and no form this scan does not model can
/// slip past it. No file or no token is a problem.
#[must_use]
pub fn override_problems(files: &[(String, String)], tokens: &[&str]) -> Vec<Problem> {
    if files.is_empty() || tokens.is_empty() {
        return vec![Problem::new(
            "no file or no token to look for; the override check would pass on nothing",
        )];
    }
    files
        .iter()
        .flat_map(|(path, text)| {
            tokens
                .iter()
                .filter(|token| text.contains(**token))
                .map(move |token| {
                    Problem::at(
                        path,
                        format!("names {token}, which could change a lint level from outside the manifest"),
                    )
                })
        })
        .collect()
}

/// The crate name in a slash-separated `<crates_dir>/<name>/Cargo.toml` path, or `None` for any
/// other path.
#[must_use]
pub fn crate_of_manifest<'path>(crates_dir: &str, path: &'path str) -> Option<&'path str> {
    let rest = path.strip_prefix(crates_dir)?.strip_prefix('/')?;
    let name = rest.strip_suffix("/Cargo.toml")?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

/// The crate directories under `crates_dir` with a tracked `<crates_dir>/<name>/Cargo.toml`.
///
/// # Panics
///
/// git cannot list them.
#[must_use]
pub fn tracked_crates(repo: &Repo, crates_dir: &str) -> Vec<String> {
    repo.tracked(&[&format!("{crates_dir}/*/Cargo.toml")])
        .iter()
        .filter_map(|path| crate_of_manifest(crates_dir, path).map(str::to_owned))
        .collect()
}

/// Checks the members of the root manifest under `repo` against `crates`, the crate directories
/// under `crates_dir` that have a tracked manifest: every member is one of `others` or
/// `<crates_dir>/<name>`, and the `<crates_dir>` members and `crates` are the same set. A member
/// elsewhere would escape a per-crate gate, and a tracked crate that is not a member every gate.
#[must_use]
pub fn layout_problems(
    repo: &Repo,
    crates_dir: &str,
    others: &[&str],
    crates: &[String],
) -> Vec<Problem> {
    let members = match read_manifest(repo, "Cargo.toml").and_then(|root| members(&root)) {
        Ok(members) => members,
        Err(problem) => return vec![problem],
    };
    let mut problems = Vec::new();
    let mut member_crates = BTreeSet::new();
    for member in &members {
        if others.contains(&member.as_str()) {
            continue;
        }
        match crate_of_manifest(crates_dir, &format!("{member}/Cargo.toml")) {
            Some(name) => {
                member_crates.insert(name.to_owned());
            }
            None => problems.push(Problem::new(format!(
                "workspace member {member:?} is neither {} nor {crates_dir}/<name>",
                others.join(" nor ")
            ))),
        }
    }
    let tracked: BTreeSet<String> = crates.iter().cloned().collect();
    for name in tracked.difference(&member_crates) {
        problems.push(Problem::new(format!(
            "{crates_dir}/{name}/Cargo.toml is tracked but {crates_dir}/{name} is not a workspace \
             member"
        )));
    }
    for name in member_crates.difference(&tracked) {
        problems.push(Problem::new(format!(
            "workspace member {crates_dir}/{name} has no tracked {crates_dir}/{name}/Cargo.toml"
        )));
    }
    problems
}

#[cfg(test)]
mod tests;
