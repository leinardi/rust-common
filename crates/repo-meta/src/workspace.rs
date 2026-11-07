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
//! `[workspace.lints.rust]` and `[workspace.lints.clippy]`, and every member is listed explicitly
//! and declares `[lints] workspace = true` (`[lints]` is not inherited, so a member that forgets
//! it builds and passes clippy with the default lints, silently).
//!
//! Extracted from swarm-device-access/meta/tests/workspace_lints.rs at 8b4ab3a3e417 (the checks as
//! functions of the manifests) and `bitgauge/meta/tests/lint_inheritance.rs` at 600fc3905200 (a `.`
//! member and the expected member list).

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

#[cfg(test)]
mod tests;
