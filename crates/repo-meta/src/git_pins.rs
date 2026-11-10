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

//! The pins of the shared crates in a consumer's manifests: a consumer depends on a crate of this
//! workspace as `name = { git = "<url>", tag = "vX.Y.Z", version = "X.Y" }`, usually one line per
//! crate under `[workspace.dependencies]`, and nothing else checks that those lines agree.
//! [`git_pins`] refuses what makes a bump fail or silently mixes two checkouts:
//!
//! - a dependency on the URL without a `tag` (a `rev`, a `branch`, or no reference at all);
//! - a `tag` that is not `vX.Y.Z`;
//! - a `version` that is missing or is not the tag's `<major>.<minor>` (a tag-only bump, by hand
//!   or by Dependabot, then fails with "failed to select a version for the requirement");
//! - tags that differ between the dependencies (cargo locks one checkout per tag);
//! - no dependency on the URL at all, since a check that found nothing would pass on nothing.
//!
//! The URL is a parameter ([`RUST_COMMON_URL`] is the one of this workspace); it matches with or
//! without a trailing `/` or `.git`. A renamed dependency (`package = "..."`) is judged like any
//! other and named by both names. The dependency tables read are `[workspace.dependencies]`,
//! `[dependencies]`, `[dev-dependencies]` and `[build-dependencies]` of each manifest given.

use std::collections::BTreeMap;

use toml::{Table, Value};

use crate::{Problem, Repo};

/// The repository URL of this workspace, as the consumers write it in `git = "..."`.
pub const RUST_COMMON_URL: &str = "https://github.com/leinardi/rust-common";

/// The dependency tables of a manifest, as paths of keys.
const TABLES: [&[&str]; 4] = [
    &["workspace", "dependencies"],
    &["dependencies"],
    &["dev-dependencies"],
    &["build-dependencies"],
];

/// Checks the pins of the dependencies on `url` in the root `Cargo.toml` under `repo`
/// ([`git_pins`] on that one manifest).
#[must_use]
pub fn root_git_pins(repo: &Repo, url: &str) -> Vec<Problem> {
    match repo.try_read("Cargo.toml") {
        Ok(text) => git_pins(&[("Cargo.toml", &text)], url),
        Err(problem) => vec![problem],
    }
}

/// Checks the pins of every dependency on the git repository `url` in the manifests, given as
/// `(file, text)` pairs: each has a `tag` of the form `vX.Y.Z` and a `version` equal to its
/// `X.Y`, and all share one tag. Dependencies on other repositories are ignored.
///
/// A manifest that does not parse is a problem, and so is none of the manifests depending on
/// `url` (or none given): the check never passes having judged nothing.
#[must_use]
pub fn git_pins(manifests: &[(&str, &str)], url: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut tags: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut found = 0_usize;
    for &(file, text) in manifests {
        let manifest = match text.parse::<Table>() {
            Ok(manifest) => manifest,
            Err(error) => {
                problems.push(Problem::at(file, format!("parse: {error}")));
                continue;
            }
        };
        let manifest = Value::Table(manifest);
        for path in TABLES {
            let Some(table) = path
                .iter()
                .try_fold(&manifest, |value, key| value.get(key))
                .and_then(Value::as_table)
            else {
                continue;
            };
            for (name, spec) in table {
                let Some(spec) = spec.as_table() else {
                    continue;
                };
                if !spec
                    .get("git")
                    .and_then(Value::as_str)
                    .is_some_and(|git| same_url(git, url))
                {
                    continue;
                }
                found += 1;
                let label = label(name, spec);
                match pin_problem(spec, &label) {
                    Ok(tag) => tags
                        .entry(tag)
                        .or_default()
                        .push(format!("{file}: {label}")),
                    Err(message) => problems.push(Problem::at(file, message)),
                }
            }
        }
    }
    if found == 0 {
        problems.push(Problem::new(format!(
            "no dependency on {url} in {}: the pin check would pass on nothing",
            files(manifests)
        )));
    }
    if tags.len() > 1 {
        let detail = tags
            .iter()
            .map(|(tag, users)| format!("{tag} ({})", users.join(", ")))
            .collect::<Vec<_>>()
            .join("; ");
        problems.push(Problem::new(format!(
            "the dependencies on {url} pin different tags, and cargo locks a checkout for each: \
             {detail}"
        )));
    }
    problems
}

fn files(manifests: &[(&str, &str)]) -> String {
    if manifests.is_empty() {
        return "no manifest".to_owned();
    }
    manifests
        .iter()
        .map(|(file, _)| *file)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A URL without a trailing slash or `.git`, so the spellings of one repository compare equal.
fn same_url(left: &str, right: &str) -> bool {
    fn normal(url: &str) -> &str {
        let url = url.trim_end_matches('/');
        url.strip_suffix(".git").unwrap_or(url)
    }
    normal(left) == normal(right)
}

/// The dependency's key, with the package it renames when that differs.
fn label(name: &str, spec: &Table) -> String {
    match spec.get("package").and_then(Value::as_str) {
        Some(package) if package != name => format!("`{name}` (package `{package}`)"),
        _ => format!("`{name}`"),
    }
}

/// The tag of a well-pinned dependency, or what is wrong with its pin.
fn pin_problem(spec: &Table, label: &str) -> Result<String, String> {
    let Some(tag) = spec.get("tag").and_then(Value::as_str) else {
        let by = ["rev", "branch"]
            .into_iter()
            .find(|key| spec.contains_key(*key))
            .map_or_else(
                || "no reference".to_owned(),
                |key| format!("`{key}` instead of `tag`"),
            );
        return Err(format!(
            "{label} is pinned by {by}; pin it with `tag = \"vX.Y.Z\"`"
        ));
    };
    let Some(line) = release_line(tag) else {
        return Err(format!(
            "{label} has tag {tag:?}, which is not of the form vX.Y.Z"
        ));
    };
    match spec.get("version").and_then(Value::as_str) {
        Some(version) if version == line => Ok(tag.to_owned()),
        Some(version) => Err(format!(
            "{label} has version {version:?} but tag {tag}: the version must be {line:?}, or a \
             tag-only bump fails to select a version"
        )),
        None => Err(format!(
            "{label} has tag {tag} and no version: add `version = {line:?}`"
        )),
    }
}

/// `X.Y` of a tag `vX.Y.Z` (decimal numbers without signs), `None` for anything else.
fn release_line(tag: &str) -> Option<String> {
    let mut parts = tag.strip_prefix('v')?.split('.');
    let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    [major, minor, patch]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("{major}.{minor}"))
}

#[cfg(test)]
mod tests;
