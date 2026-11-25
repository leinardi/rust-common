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

//! The boundary table of `repo_meta::boundary` holds to the crates: every third-party dependency
//! of every crate is classified, every root re-export of an external crate is a `Reexport` entry
//! and the reverse, and no entry names a feature the crate lacks or a package outside its locked
//! closure. A new dependency or re-export is so a decision recorded in the table.

use std::collections::{BTreeMap, BTreeSet};

use repo_meta::boundary::{Entry, Role};
use repo_meta::workspace::members;
use repo_meta::{Problem, Repo};
use toml::{Table, Value};

/// One crate under `crates/`, as its manifest declares it.
struct Crate {
    directory: String,
    name: String,
    lib: String,
    features: BTreeMap<String, Vec<String>>,
    /// Third-party normal dependencies (target-specific ones included): (key, package, optional).
    dependencies: Vec<(String, String, bool)>,
}

fn table<'value>(value: &'value Table, key: &str) -> Option<&'value Table> {
    value.get(key).and_then(Value::as_table)
}

fn read_crate(repo: &Repo, directory: &str) -> Result<Crate, Problem> {
    let path = format!("{directory}/Cargo.toml");
    let manifest: Table = repo
        .try_read(&path)?
        .parse()
        .map_err(|error| Problem::at(&path, format!("parse: {error}")))?;
    let name = table(&manifest, "package")
        .and_then(|package| package.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| Problem::at(&path, "no [package] name"))?
        .to_owned();
    let lib = table(&manifest, "lib")
        .and_then(|lib| lib.get("name"))
        .and_then(Value::as_str)
        .map_or_else(|| name.replace('-', "_"), str::to_owned);
    let features = table(&manifest, "features")
        .map(|features| {
            features
                .iter()
                .map(|(feature, enables)| {
                    let enables = enables
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    (feature.clone(), enables)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut sections: Vec<&Table> = table(&manifest, "dependencies").into_iter().collect();
    for target in table(&manifest, "target")
        .into_iter()
        .flat_map(Table::values)
    {
        sections.extend(
            target
                .as_table()
                .and_then(|target| table(target, "dependencies")),
        );
    }
    let mut dependencies = Vec::new();
    for section in sections {
        for (key, value) in section {
            let detail = value.as_table();
            if detail.is_some_and(|detail| detail.contains_key("path")) {
                continue;
            }
            let package = detail
                .and_then(|detail| detail.get("package"))
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_owned();
            let optional = detail
                .and_then(|detail| detail.get("optional"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            dependencies.push((key.clone(), package, optional));
        }
    }
    Ok(Crate {
        directory: directory.to_owned(),
        name,
        lib,
        features,
        dependencies,
    })
}

/// The package names each package of `Cargo.lock` depends on, by name.
fn lock_edges(text: &str) -> Result<BTreeMap<String, BTreeSet<String>>, Problem> {
    let lock: Table = text
        .parse()
        .map_err(|error| Problem::at("Cargo.lock", format!("parse: {error}")))?;
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in lock
        .get("package")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_table)
    {
        let Some(name) = package.get("name").and_then(Value::as_str) else {
            continue;
        };
        let targets = package
            .get("dependencies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|dependency| dependency.split(' ').next())
            .map(str::to_owned);
        edges.entry(name.to_owned()).or_default().extend(targets);
    }
    Ok(edges)
}

fn closure(edges: &BTreeMap<String, BTreeSet<String>>, start: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![start.to_owned()];
    while let Some(name) = stack.pop() {
        for next in edges.get(&name).into_iter().flatten() {
            if seen.insert(next.clone()) {
                stack.push(next.clone());
            }
        }
    }
    seen
}

/// The `pub use <ident>;` lines of a crate root, with the feature of a `#[cfg(feature = "...")]`
/// right above (doc comments in between allowed).
fn root_reexports(text: &str) -> Vec<(usize, String, Option<String>)> {
    let cfg = repo_meta::pattern(r#"^#\[cfg\(feature = "([^"]+)"\)\]$"#);
    let reexport = repo_meta::pattern(r"^pub use ([a-z_][a-z0-9_]*);$");
    let mut found = Vec::new();
    let mut feature: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if let Some(captures) = cfg.captures(line) {
            feature = captures.get(1).map(|feature| feature.as_str().to_owned());
        } else if let Some(captures) = reexport.captures(line) {
            if let Some(ident) = captures.get(1) {
                found.push((index + 1, ident.as_str().to_owned(), feature.take()));
            }
        } else if !line.starts_with("///") && !line.starts_with("#[") {
            feature = None;
        }
    }
    found
}

fn describe(entry: &Entry) -> String {
    format!(
        "({}, {}, {})",
        entry.krate,
        entry.feature.unwrap_or("always"),
        entry.package
    )
}

/// Every way `table` and the crates under `repo` disagree.
#[must_use]
pub fn completeness(repo: &Repo, table: &[Entry]) -> Vec<Problem> {
    let members = match repo
        .try_read("Cargo.toml")
        .and_then(|text| {
            text.parse::<Table>()
                .map_err(|error| Problem::at("Cargo.toml", format!("parse: {error}")))
        })
        .and_then(|root| members(&root))
    {
        Ok(members) => members,
        Err(problem) => return vec![problem],
    };
    let edges = match repo
        .try_read("Cargo.lock")
        .and_then(|text| lock_edges(&text))
    {
        Ok(edges) => edges,
        Err(problem) => return vec![problem],
    };
    let mut problems = Vec::new();
    let mut crates = Vec::new();
    for directory in members
        .iter()
        .filter(|member| member.starts_with("crates/"))
    {
        match read_crate(repo, directory) {
            Ok(krate) => crates.push(krate),
            Err(problem) => problems.push(problem),
        }
    }
    if crates.is_empty() {
        problems.push(Problem::new(
            "no crate under crates/: the check would pass on nothing",
        ));
        return problems;
    }
    let names: BTreeSet<&str> = crates.iter().map(|krate| krate.name.as_str()).collect();
    for entry in table {
        if !names.contains(entry.krate) {
            problems.push(Problem::new(format!(
                "boundary entry {} names a crate this workspace lacks",
                describe(entry)
            )));
        }
    }
    for krate in &crates {
        let entries: Vec<&Entry> = table
            .iter()
            .filter(|entry| entry.krate == krate.name)
            .collect();
        problems.extend(unclassified(krate, &entries));
        problems.extend(stale(krate, &entries, &closure(&edges, &krate.name)));
        match repo.try_read(&format!("{}/src/lib.rs", krate.directory)) {
            Ok(text) => problems.extend(reexports(krate, &entries, &text)),
            Err(problem) => problems.push(problem),
        }
    }
    problems
}

/// Every dependency without an entry, under every feature that brings it in.
fn unclassified(krate: &Crate, entries: &[&Entry]) -> Vec<Problem> {
    let manifest = format!("{}/Cargo.toml", krate.directory);
    let mut problems = Vec::new();
    for (key, package, optional) in &krate.dependencies {
        let features: Vec<Option<&str>> = if *optional {
            let enabling: Vec<Option<&str>> = krate
                .features
                .iter()
                .filter(|(_, enables)| enables.iter().any(|item| *item == format!("dep:{key}")))
                .map(|(feature, _)| Some(feature.as_str()))
                .collect();
            if enabling.is_empty() {
                vec![Some(key.as_str())]
            } else {
                enabling
            }
        } else {
            vec![None]
        };
        for feature in features {
            if !entries
                .iter()
                .any(|entry| entry.package == package && entry.feature == feature)
            {
                problems.push(Problem::at(
                    &manifest,
                    format!(
                        "{package} is not in the boundary table under {}{}: classify it as \
                         Reexport, Guarded or Internal (repo_meta::boundary::TABLE)",
                        krate.name,
                        feature
                            .map(|feature| format!(" (feature {feature})"))
                            .unwrap_or_default()
                    ),
                ));
            }
        }
    }
    problems
}

/// Every entry that names a feature the crate lacks or a package it does not reach.
fn stale(krate: &Crate, entries: &[&Entry], reached: &BTreeSet<String>) -> Vec<Problem> {
    let mut problems = Vec::new();
    for entry in entries {
        if let Some(feature) = entry.feature
            && !krate.features.contains_key(feature)
        {
            problems.push(Problem::new(format!(
                "boundary entry {} names a feature {} does not declare",
                describe(entry),
                krate.name
            )));
        }
        if !reached.contains(entry.package) {
            problems.push(Problem::new(format!(
                "boundary entry {}: {} does not depend on {} in Cargo.lock",
                describe(entry),
                krate.name,
                entry.package
            )));
        }
    }
    problems
}

/// Root re-exports of external crates and `Reexport` entries are the same set.
fn reexports(krate: &Crate, entries: &[&Entry], lib_text: &str) -> Vec<Problem> {
    let lib_rs = format!("{}/src/lib.rs", krate.directory);
    let externals: BTreeMap<String, &str> = krate
        .dependencies
        .iter()
        .map(|(key, package, _)| (key.replace('-', "_"), package.as_str()))
        .collect();
    let mut problems = Vec::new();
    let mut listed = BTreeSet::new();
    for (line, ident, feature) in root_reexports(lib_text) {
        let Some(package) = externals.get(&ident) else {
            continue;
        };
        let path = format!("{}::{ident}", krate.lib);
        let matches = entries.iter().any(|entry| {
            entry.package == *package
                && matches!(entry.role, Role::Reexport(entry_path) if entry_path == path)
                && entry.feature == feature.as_deref()
        });
        if !matches {
            problems.push(Problem::at(
                format!("{lib_rs}:{line}"),
                format!(
                    "pub use {ident} re-exports {package}{} with no Reexport(\"{path}\") entry \
                     under that feature in the boundary table",
                    feature
                        .as_deref()
                        .map(|feature| format!(" under feature {feature}"))
                        .unwrap_or_default()
                ),
            ));
        }
        listed.insert(path);
    }
    for entry in entries {
        if let Role::Reexport(path) = entry.role
            && !listed.contains(path)
        {
            problems.push(Problem::new(format!(
                "boundary entry {} is Reexport({path:?}), but {lib_rs} has no pub use of it at \
                 its root",
                describe(entry)
            )));
        }
    }
    problems
}
