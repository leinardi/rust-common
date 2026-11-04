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

//! The workspace's shape: the root manifest declares the shared lint set, every member opts into
//! it, and every member is either the meta crate or a library crate under `crates/<name>`, with
//! every tracked `crates/*/Cargo.toml` a member. Each check takes the root, so the meta-tests run
//! it on this repository and, as negative controls, on scratch trees.
//!
//! The lint check is extracted from swarm-scheduler-exporter/meta/tests/workspace_lints.rs at
//! c9e79afa8810, turned from assertions into a list of problems.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use toml::{Table, Value};

/// The one member that is not a library crate.
pub const META: &str = "meta";

fn read_manifest(root: &Path, relative: &str) -> Result<Table, String> {
    let text = fs::read_to_string(root.join(relative))
        .map_err(|error| format!("read {relative}: {error}"))?;
    text.parse::<Table>()
        .map_err(|error| format!("parse {relative}: {error}"))
}

/// The `[workspace] members` of the root manifest under `root`, as written.
///
/// # Errors
///
/// The root manifest cannot be read or parsed, or lists no members, or a member is not a string.
pub fn members(root: &Path) -> Result<Vec<String>, String> {
    let manifest = read_manifest(root, "Cargo.toml")?;
    let members = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(Value::as_array)
        .ok_or("the root Cargo.toml lists no [workspace] members")?;
    if members.is_empty() {
        return Err("the workspace lists no members".to_owned());
    }
    members
        .iter()
        .map(|member| {
            member
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("workspace member {member} is not a path string"))
        })
        .collect()
}

/// Checks that the root manifest under `root` declares `[workspace.lints.rust]` and
/// `[workspace.lints.clippy]`, and that every member is listed explicitly (no glob) and declares
/// `[lints] workspace = true`: `[lints]` is not inherited, so a member that forgets it builds and
/// passes clippy with the default lints, silently.
#[must_use]
pub fn lint_problems(root: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    match read_manifest(root, "Cargo.toml") {
        Ok(manifest) => {
            let lints = manifest
                .get("workspace")
                .and_then(|workspace| workspace.get("lints"))
                .and_then(Value::as_table);
            for table in ["rust", "clippy"] {
                if !lints.is_some_and(|lints| lints.contains_key(table)) {
                    problems.push(format!("[workspace.lints.{table}] is missing"));
                }
            }
        }
        Err(error) => return vec![error],
    }
    let members = match members(root) {
        Ok(members) => members,
        Err(error) => return vec![error],
    };
    for member in members {
        if member.contains('*') {
            problems.push(format!(
                "member {member:?} is a glob; list members explicitly so this check sees each one"
            ));
            continue;
        }
        let path = format!("{member}/Cargo.toml");
        match read_manifest(root, &path) {
            Ok(manifest) => {
                let opts_in = manifest
                    .get("lints")
                    .and_then(|lints| lints.get("workspace"))
                    .and_then(Value::as_bool);
                if opts_in != Some(true) {
                    problems.push(format!("{path} does not declare [lints] workspace = true"));
                }
            }
            Err(error) => problems.push(error),
        }
    }
    problems
}

/// The crate name in a slash-separated `crates/<name>/Cargo.toml` path, or `None` for any other
/// path.
#[must_use]
pub fn crate_of_manifest(path: &str) -> Option<&str> {
    match path.split('/').collect::<Vec<_>>().as_slice() {
        ["crates", name, "Cargo.toml"] => Some(name),
        _ => None,
    }
}

/// Checks the members under `root` against `crates`, the crate directories under `crates/` that
/// have a tracked manifest: every member is `meta` or `crates/<name>`, and the `crates/` members
/// and `crates` are the same set. A member elsewhere would escape the feature matrix and the
/// coverage check, and a tracked crate that is not a member would escape every gate.
#[must_use]
pub fn layout_problems(root: &Path, crates: &[String]) -> Vec<String> {
    let members = match members(root) {
        Ok(members) => members,
        Err(error) => return vec![error],
    };
    let mut problems = Vec::new();
    let mut member_crates = BTreeSet::new();
    for member in &members {
        if member == META {
            continue;
        }
        match crate_of_manifest(&format!("{member}/Cargo.toml")) {
            Some(name) => {
                member_crates.insert(name.to_owned());
            }
            None => problems.push(format!(
                "workspace member {member:?} is neither {META} nor crates/<name>"
            )),
        }
    }
    let tracked: BTreeSet<String> = crates.iter().cloned().collect();
    for name in tracked.difference(&member_crates) {
        problems.push(format!(
            "crates/{name}/Cargo.toml is tracked but crates/{name} is not a workspace member"
        ));
    }
    for name in member_crates.difference(&tracked) {
        problems.push(format!(
            "workspace member crates/{name} has no tracked crates/{name}/Cargo.toml"
        ));
    }
    problems
}
