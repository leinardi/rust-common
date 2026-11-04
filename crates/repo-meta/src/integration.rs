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

//! The line between the plain `cargo test` and a Docker-driving integration suite: the plain test
//! targets (`make rust-test`, the cargo-test hook, the rust CI job) run without Docker, so
//! everything that reaches a daemon sits behind the harness's feature, every harness test target
//! requires that feature, no other source drives Docker, and no plain test command turns the
//! feature on.
//!
//! Extracted from swarm-device-access/meta/tests/integration_targets.rs at 8b4ab3a3e417 (every
//! harness source outside a gated module scanned, lib.rs included; every test target held to the
//! feature; the plain test commands), itself a superset of
//! swarm-scheduler-exporter/meta/tests/integration_targets.rs at c9e79afa8810.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use toml::Table;

use crate::{Problem, pattern, regex};

/// A module declaration in lib.rs, with the attribute lines right above it.
static MODULE: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?m)^((?:#\[[^\n]*\]\n)*)(?:pub(?:\([a-z]+\))? )?mod ([a-z_][a-z0-9_]*);")
});
/// A cargo test command: cargo itself or make's `$(CARGO)`.
static CARGO_TEST: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)\bcargo\)?\s+(\+\S+\s+)?test\b"));

/// A harness whose Docker-driving code sits behind a feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntegrationGate<'gate> {
    /// The feature, such as `integration`.
    pub feature: &'gate str,
    /// The harness's source directory, such as `testenv/src`; its `lib.rs` declares the modules.
    pub src_dir: &'gate str,
    /// The harness's manifest, such as `testenv/Cargo.toml`.
    pub manifest: &'gate str,
    /// The test target the suite is, which must exist, such as `integration`.
    pub suite: &'gate str,
    /// The only files that may run a test command with the feature on: the integration make
    /// fragment and workflow.
    pub integration_files: &'gate [&'gate str],
}

impl IntegrationGate<'_> {
    /// The modules `lib` (the harness's `lib.rs`) declares behind the feature gate.
    #[must_use]
    pub fn gated_modules(&self, lib: &str) -> BTreeSet<String> {
        let gate = format!("#[cfg(feature = \x22{}\x22)]", self.feature);
        MODULE
            .captures_iter(lib)
            .filter(|captures| captures[1].contains(&gate))
            .map(|captures| captures[2].to_owned())
            .collect()
    }

    /// The harness sources of `paths` built without the feature: every file under the source
    /// directory whose top-level module is not gated in `lib`, lib.rs itself included.
    #[must_use]
    pub fn ungated_sources(&self, lib: &str, paths: &[String]) -> Vec<String> {
        let gated = self.gated_modules(lib);
        let prefix = format!("{}/", self.src_dir.trim_end_matches('/'));
        paths
            .iter()
            .filter(|path| {
                let module = path
                    .strip_prefix(&prefix)
                    .unwrap_or(path)
                    .split(['/', '.'])
                    .next()
                    .unwrap_or_default();
                !gated.contains(module)
            })
            .cloned()
            .collect()
    }

    /// What is wrong with the harness manifest (`manifest`, its text): the feature must exist and
    /// be off by default, the suite must be a test target, and every `[[test]]` target must
    /// require exactly the feature.
    #[must_use]
    pub fn manifest_problems(&self, manifest: &str) -> Vec<Problem> {
        let file = self.manifest;
        let feature = self.feature;
        let manifest: Table = match manifest.parse() {
            Ok(manifest) => manifest,
            Err(error) => return vec![Problem::at(file, format!("parse: {error}"))],
        };
        let mut problems = Vec::new();
        let features = manifest.get("features").and_then(toml::Value::as_table);
        if !features.is_some_and(|features| features.contains_key(feature)) {
            problems.push(Problem::at(file, format!("declares no {feature} feature")));
        }
        let default_on = features
            .and_then(|features| features.get("default"))
            .and_then(toml::Value::as_array)
            .is_some_and(|defaults| defaults.iter().any(|item| item.as_str() == Some(feature)));
        if default_on {
            problems.push(Problem::at(file, format!("turns {feature} on by default")));
        }
        let targets = manifest
            .get("test")
            .and_then(toml::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !targets
            .iter()
            .any(|target| target.get("name").and_then(toml::Value::as_str) == Some(self.suite))
        {
            problems.push(Problem::at(
                file,
                format!("declares no test target named {}", self.suite),
            ));
        }
        for target in &targets {
            let name = target
                .get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or("<unnamed>");
            let required: Vec<&str> = target
                .get("required-features")
                .and_then(toml::Value::as_array)
                .map(|features| features.iter().filter_map(toml::Value::as_str).collect())
                .unwrap_or_default();
            if required != [feature] {
                problems.push(Problem::at(
                    file,
                    format!(
                        "the test target {name} requires {required:?}, want exactly [{feature:?}]"
                    ),
                ));
            }
        }
        problems
    }

    /// The test commands of `content` (the text of `file`: a make fragment, the pre-commit
    /// configuration or a workflow) that turn the feature on, all features included, unless
    /// `file` is one of the integration files.
    #[must_use]
    pub fn plain_test_problems(&self, file: &str, content: &str) -> Vec<Problem> {
        if self.integration_files.contains(&file) {
            return Vec::new();
        }
        let enables = pattern(&format!(
            r"--all-features\b|(?:--features[ =]|-F\s*)\S*\b{}\b",
            regex::escape(self.feature)
        ));
        content
            .lines()
            .enumerate()
            .filter(|(_, line)| CARGO_TEST.is_match(line) && enables.is_match(line))
            .map(|(index, line)| {
                Problem::at(
                    format!("{file}:{}", index + 1),
                    format!("turns {} on in a plain test: {}", self.feature, line.trim()),
                )
            })
            .collect()
    }
}

/// The cargo test commands of `content`: how many a scan saw, so it can refuse to pass on none.
#[must_use]
pub fn cargo_test_commands(content: &str) -> usize {
    content
        .lines()
        .filter(|line| CARGO_TEST.is_match(line))
        .count()
}

/// The files of `files` (path and text) that drive Docker: one of the `clients` crates imported
/// or named in code (not in a comment or a string, as a boundary test's own fixtures do), or the
/// docker CLI started as a child. No file or no client is a problem.
#[must_use]
pub fn docker_drivers(files: &[(String, String)], clients: &[&str]) -> Vec<Problem> {
    if files.is_empty() || clients.is_empty() {
        return vec![Problem::new(
            "no file or no Docker client to look for; the check would pass on nothing",
        )];
    }
    let names = clients
        .iter()
        .map(|client| regex::escape(client))
        .collect::<Vec<_>>()
        .join("|");
    let drives = pattern(&format!(
        r"(?m)^\s*(?:pub )?use (?:{names})\b|^[^\x22/\n]*\b(?:{names})::|Command::new\(\s*\x22docker\x22"
    ));
    files
        .iter()
        .filter(|(_, text)| drives.is_match(text))
        .map(|(path, _)| Problem::at(path, "drives Docker outside the gated harness"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{IntegrationGate, cargo_test_commands, docker_drivers};

    const GATE: IntegrationGate<'static> = IntegrationGate {
        feature: "integration",
        src_dir: "testenv/src",
        manifest: "testenv/Cargo.toml",
        suite: "integration",
        integration_files: &[
            ".mk/integration-test.mk",
            ".github/workflows/integration.yaml",
        ],
    };

    #[test]
    fn modules_and_sources() {
        let lib = "#[cfg(feature = \x22integration\x22)]\nmod dind;\n\
                   #[cfg(feature = \x22integration\x22)]\npub mod context;\n\
                   mod error;\npub mod redact;\n#[cfg(test)]\nmod tests;\n";
        assert_eq!(
            GATE.gated_modules(lib),
            BTreeSet::from(["context".to_owned(), "dind".to_owned()])
        );
        let paths: Vec<String> = [
            "testenv/src/lib.rs",
            "testenv/src/dind.rs",
            "testenv/src/context/mod.rs",
            "testenv/src/error.rs",
            "testenv/src/redact/tests.rs",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            GATE.ungated_sources(lib, &paths),
            [
                "testenv/src/lib.rs",
                "testenv/src/error.rs",
                "testenv/src/redact/tests.rs"
            ]
        );
    }

    #[test]
    fn drivers_are_found_in_code_only() {
        let files = [
            ("a.rs".to_owned(), "use bollard::Docker;".to_owned()),
            (
                "b.rs".to_owned(),
                "let out = Command::new(\x22docker\x22).arg(\x22ps\x22);".to_owned(),
            ),
            (
                "c.rs".to_owned(),
                "let out = Command::new(\x22git\x22); // docker".to_owned(),
            ),
            (
                "d.rs".to_owned(),
                "let fixture = \x22use bollard::Docker;\x22; // bollard::Docker".to_owned(),
            ),
            (
                "e.rs".to_owned(),
                "    let client = bollard::Docker::connect();".to_owned(),
            ),
            ("f.rs".to_owned(), "use docker_engine::Client;".to_owned()),
        ];
        let found: Vec<String> = docker_drivers(&files, &["bollard"])
            .into_iter()
            .map(|problem| problem.location)
            .collect();
        assert_eq!(found, ["a.rs", "b.rs", "e.rs"]);
        assert_eq!(
            docker_drivers(&files, &["bollard", "docker_engine"]).len(),
            4
        );
    }

    #[test]
    fn manifests_keep_the_suite_behind_the_feature() {
        let good = "[features]\nintegration = []\n\n[[test]]\nname = \x22integration\x22\n\
                    required-features = [\x22integration\x22]\n";
        assert_eq!(GATE.manifest_problems(good), []);
        for (name, manifest) in [
            ("no feature", "[features]\n".to_owned()),
            (
                "on by default",
                "[features]\ndefault = [\x22integration\x22]\nintegration = []\n\n[[test]]\n\
                 name = \x22integration\x22\nrequired-features = [\x22integration\x22]\n"
                    .to_owned(),
            ),
            (
                "no integration test target",
                "[features]\nintegration = []\n".to_owned(),
            ),
            (
                "a test target without the feature",
                good.replace("required-features = [\x22integration\x22]\n", ""),
            ),
            (
                "a test target with another feature",
                good.replace(
                    "[\x22integration\x22]\n",
                    "[\x22integration\x22, \x22x\x22]\n",
                ),
            ),
        ] {
            assert!(
                !GATE.manifest_problems(&manifest).is_empty(),
                "{name} was accepted"
            );
        }
    }

    #[test]
    fn plain_test_commands_leave_the_feature_off() {
        for line in [
            "\t$(CARGO) test --workspace --locked --all-features",
            "        run: cargo test --workspace --locked --features integration",
            "        entry: cargo test --locked -p testenv -F testenv/integration",
            "\tcargo test --features=integration",
        ] {
            assert_eq!(
                GATE.plain_test_problems(".mk/rust.mk", line).len(),
                1,
                "{line:?} was accepted in a plain target"
            );
            assert!(
                GATE.plain_test_problems(".mk/integration-test.mk", line)
                    .is_empty(),
                "{line:?} was refused in the integration fragment"
            );
        }
        for line in [
            "\t$(CARGO) test --workspace --locked",
            "\t$(CARGO) clippy --workspace --all-targets --all-features --locked -- -D warnings",
            "        entry: cargo test --locked -p meta --test docs",
        ] {
            assert!(
                GATE.plain_test_problems(".mk/rust.mk", line).is_empty(),
                "{line:?} was refused"
            );
        }
        assert_eq!(
            cargo_test_commands("\tcargo test\n\t$(CARGO) test -p x\n\tcargo build\n"),
            2
        );
    }

    /// No file or no client is a problem, not a pass on nothing.
    #[test]
    fn docker_drivers_need_files_and_clients() {
        let files = [("src/x.rs".to_owned(), "use bollard::Docker;\n".to_owned())];
        for (files, clients) in [(&files[..], &[][..]), (&[][..], &["bollard"][..])] {
            assert_eq!(
                crate::strings(&super::docker_drivers(files, clients)),
                ["no file or no Docker client to look for; the check would pass on nothing"]
            );
        }
    }
}
