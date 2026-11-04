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

//! `feature-sets.toml`, the per-crate feature matrix: its reader, the cargo commands
//! `make rust-feature-matrix` runs from it, and the check that it covers every feature the crates
//! declare. The format is documented at the top of the file itself.
//!
//! Every check here refuses rather than passes on nothing it can see: an unknown key, a flag set
//! to `false`, an empty list or a crate whose manifest is missing is an error, never a set that
//! silently builds the default features.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::Path;

use toml::{Table, Value};

/// The file, relative to the repository root.
pub const FILE: &str = "feature-sets.toml";

/// One feature set of one crate: the flags of one `cargo build` and `cargo test` pair.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct FeatureSet {
    /// `--no-default-features`.
    pub no_default_features: bool,
    /// `--features a,b`, in the order the file lists them.
    pub features: Vec<String>,
    /// `--all-features`.
    pub all_features: bool,
}

impl FeatureSet {
    /// The cargo flags that select this set.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.no_default_features {
            args.push("--no-default-features".to_owned());
        }
        if !self.features.is_empty() {
            args.push("--features".to_owned());
            args.push(self.features.join(","));
        }
        if self.all_features {
            args.push("--all-features".to_owned());
        }
        args
    }
}

impl fmt::Display for FeatureSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let args = self.args();
        if args.is_empty() {
            formatter.write_str("(default features)")
        } else {
            formatter.write_str(&args.join(" "))
        }
    }
}

/// One crate's entry: its name (the directory under `crates/` and the package name) and its sets,
/// in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrateSets {
    /// The crate.
    pub name: String,
    /// Its feature sets, never empty.
    pub sets: Vec<FeatureSet>,
}

/// The manifest of the crate `name`, relative to the repository root.
#[must_use]
pub fn manifest(name: &str) -> String {
    format!("crates/{name}/Cargo.toml")
}

/// Reads the text of a `feature-sets.toml`.
///
/// # Errors
///
/// Every problem found, one message each: invalid TOML, a crate name that is not a plain
/// directory name, a value that is not a non-empty array of tables, an unknown key, a flag that is
/// not `true`, an empty or repeated feature list, a set repeated within a crate, and
/// `all-features` combined with another key.
pub fn parse(text: &str) -> Result<Vec<CrateSets>, Vec<String>> {
    let table: Table = text
        .parse()
        .map_err(|error| vec![format!("{FILE} is not valid TOML: {error}")])?;
    let mut problems = Vec::new();
    let mut crates = Vec::new();
    for (name, value) in &table {
        if !is_crate_name(name) {
            problems.push(format!(
                "{FILE}: {name:?} is not a crate directory name (lower-case letters, digits, - and _)"
            ));
            continue;
        }
        let Some(entries) = value.as_array() else {
            problems.push(format!("{FILE}: {name} is not an array of feature sets"));
            continue;
        };
        if entries.is_empty() {
            problems.push(format!(
                "{FILE}: {name} lists no feature set; use {{}} for the default features"
            ));
            continue;
        }
        let mut sets = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let at = format!("{FILE}: {name}[{index}]");
            match parse_set(entry) {
                Ok(set) if sets.contains(&set) => {
                    problems.push(format!("{at}: repeats the set {set}"));
                }
                Ok(set) => sets.push(set),
                Err(messages) => {
                    problems.extend(
                        messages
                            .into_iter()
                            .map(|message| format!("{at}: {message}")),
                    );
                }
            }
        }
        crates.push(CrateSets {
            name: name.clone(),
            sets,
        });
    }
    if problems.is_empty() {
        Ok(crates)
    } else {
        Err(problems)
    }
}

fn is_crate_name(name: &str) -> bool {
    name.starts_with(|first: char| first.is_ascii_lowercase() || first.is_ascii_digit())
        && name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
}

fn parse_set(entry: &Value) -> Result<FeatureSet, Vec<String>> {
    let Some(table) = entry.as_table() else {
        return Err(vec!["is not a table".to_owned()]);
    };
    let mut problems = Vec::new();
    let mut set = FeatureSet::default();
    for (key, value) in table {
        match key.as_str() {
            "no-default-features" => {
                set.no_default_features = true_flag(key, value, &mut problems);
            }
            "all-features" => set.all_features = true_flag(key, value, &mut problems),
            "features" => set.features = feature_list(value, &mut problems),
            _ => problems.push(format!(
                "unknown key {key:?} (no-default-features, features, all-features)"
            )),
        }
    }
    if set.all_features && (set.no_default_features || !set.features.is_empty()) {
        problems.push(
            "all-features enables every feature; combined with another key it hides what the set \
             means"
                .to_owned(),
        );
    }
    if problems.is_empty() {
        Ok(set)
    } else {
        Err(problems)
    }
}

/// A flag is either `true` or absent: `false` would select nothing and read as if it did.
fn true_flag(key: &str, value: &Value, problems: &mut Vec<String>) -> bool {
    if value.as_bool() == Some(true) {
        true
    } else {
        problems.push(format!("{key} must be true, or left out"));
        false
    }
}

fn feature_list(value: &Value, problems: &mut Vec<String>) -> Vec<String> {
    let Some(items) = value.as_array() else {
        problems.push("features is not an array of feature names".to_owned());
        return Vec::new();
    };
    if items.is_empty() {
        problems.push("features is empty; use {} for the default features".to_owned());
    }
    let mut features: Vec<String> = Vec::new();
    for item in items {
        match item.as_str() {
            Some(feature) if feature.is_empty() || feature.contains([',', ' ']) => {
                problems.push(format!("{feature:?} is not a feature name"));
            }
            Some(feature) if features.iter().any(|known| known == feature) => {
                problems.push(format!("feature {feature:?} is listed twice"));
            }
            Some(feature) => features.push(feature.to_owned()),
            None => problems.push(format!("{item} is not a feature name")),
        }
    }
    features
}

/// Reads and parses the `feature-sets.toml` under `root`.
///
/// # Errors
///
/// The file cannot be read, or [`parse`] refuses it.
pub fn load(root: &Path) -> Result<Vec<CrateSets>, Vec<String>> {
    let text = fs::read_to_string(root.join(FILE))
        .map_err(|error| vec![format!("read {FILE}: {error}")])?;
    parse(&text)
}

/// The cargo commands of the matrix for the repository at `root`, in file order: for each crate
/// and each of its sets, a `build` and then a `test` of that one package, through its own
/// manifest. Empty when the file lists no crate.
///
/// # Errors
///
/// [`load`] fails, or a listed crate has no manifest: every such crate is named, and nothing is
/// planned.
pub fn plan(root: &Path) -> Result<Vec<Vec<String>>, Vec<String>> {
    let crates = load(root)?;
    let missing: Vec<String> = crates
        .iter()
        .map(|entry| manifest(&entry.name))
        .filter(|path| !root.join(path).is_file())
        .map(|path| format!("{FILE} lists a crate whose manifest {path} does not exist"))
        .collect();
    if !missing.is_empty() {
        return Err(missing);
    }
    let mut commands = Vec::new();
    for entry in &crates {
        let manifest = manifest(&entry.name);
        for set in &entry.sets {
            for command in ["build", "test"] {
                let mut args = vec![command.to_owned(), "--locked".to_owned()];
                args.extend([
                    "--manifest-path".to_owned(),
                    manifest.clone(),
                    "-p".to_owned(),
                    entry.name.clone(),
                ]);
                args.extend(set.args());
                commands.push(args);
            }
        }
    }
    Ok(commands)
}

/// The features a crate manifest declares: the keys of its `[features]` table other than
/// `default`, and the implicit feature of each optional dependency that no feature names as
/// `dep:<name>`.
///
/// # Errors
///
/// The manifest is not valid TOML.
pub fn declared_features(manifest_text: &str) -> Result<BTreeSet<String>, String> {
    let manifest: Table = manifest_text.parse().map_err(|error| format!("{error}"))?;
    let mut declared = BTreeSet::new();
    let mut explicit_deps = BTreeSet::new();
    if let Some(features) = manifest.get("features").and_then(Value::as_table) {
        for (name, enables) in features {
            if name != "default" {
                declared.insert(name.clone());
            }
            for item in enables.as_array().into_iter().flatten() {
                if let Some(dependency) = item.as_str().and_then(|text| text.strip_prefix("dep:")) {
                    explicit_deps.insert(dependency.to_owned());
                }
            }
        }
    }
    let mut tables: Vec<&Table> = Vec::new();
    for key in ["dependencies", "build-dependencies"] {
        tables.extend(manifest.get(key).and_then(Value::as_table));
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for target in targets.values() {
            for key in ["dependencies", "build-dependencies"] {
                tables.extend(target.get(key).and_then(Value::as_table));
            }
        }
    }
    for table in tables {
        for (name, spec) in table {
            let optional = spec.get("optional").and_then(Value::as_bool) == Some(true);
            if optional && !explicit_deps.contains(name) {
                declared.insert(name.clone());
            }
        }
    }
    Ok(declared)
}

/// Checks the `feature-sets.toml` under `root` against `crates`, the crate directories under
/// `crates/` that exist: every crate has an entry with a `--no-default-features` set and an
/// `--all-features` set, every feature it declares is named in at least one set, every feature a
/// set names is one it declares, and every listed crate is one of `crates`.
#[must_use]
pub fn coverage_problems(root: &Path, crates: &[String]) -> Vec<String> {
    let entries = match load(root) {
        Ok(entries) => entries,
        Err(problems) => return problems,
    };
    let mut problems = Vec::new();
    for entry in &entries {
        if !crates.contains(&entry.name) {
            problems.push(format!(
                "{FILE} lists {}, which is not a crate under crates/",
                entry.name
            ));
        }
    }
    for name in crates {
        let path = manifest(name);
        let declared = match fs::read_to_string(root.join(&path))
            .map_err(|error| error.to_string())
            .and_then(|text| declared_features(&text))
        {
            Ok(declared) => declared,
            Err(error) => {
                problems.push(format!("{path}: {error}"));
                continue;
            }
        };
        let Some(entry) = entries.iter().find(|entry| entry.name == *name) else {
            problems.push(format!("{FILE} has no entry for crate {name}"));
            continue;
        };
        let bare_no_default = FeatureSet {
            no_default_features: true,
            ..FeatureSet::default()
        };
        if !entry.sets.contains(&bare_no_default) {
            problems.push(format!(
                "{FILE}: {name} has no {{ no-default-features = true }} set"
            ));
        }
        let all = FeatureSet {
            all_features: true,
            ..FeatureSet::default()
        };
        if !entry.sets.contains(&all) {
            problems.push(format!(
                "{FILE}: {name} has no {{ all-features = true }} set"
            ));
        }
        let named: BTreeSet<&String> = entry.sets.iter().flat_map(|set| &set.features).collect();
        for feature in &declared {
            if !named.contains(feature) {
                problems.push(format!(
                    "{FILE}: {name} declares feature {feature:?}, which no set names"
                ));
            }
        }
        for feature in named {
            if !declared.contains(feature) {
                problems.push(format!(
                    "{FILE}: {name} names feature {feature:?}, which {path} does not declare"
                ));
            }
        }
    }
    problems
}
