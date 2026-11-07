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

//! The CI checks' negative controls.
//!
//! Extracted from swarm-device-access/meta/tests/ci_gates.rs at 8b4ab3a3e417
//! (`ci_structure_rules`).

use std::fmt::Write as _;

use super::CiGates;
use crate::strings;
use crate::yaml::{jobs, parse, pre_commit_hooks};

const GATES: CiGates<'static> = CiGates {
    result_job: "ci-result",
    pre_commit_job: "pre-commit-all",
    annotate_only: &["markdownlint"],
    skipped_hook_runners: &[("cargo-test", "rust")],
};

fn hooks(ids: &[&str]) -> Vec<(String, crate::yaml::Value)> {
    let mut config = "repos:\n  - repo: local\n    hooks:\n".to_owned();
    for id in ids {
        writeln!(config, "      - id: {id}").unwrap();
    }
    pre_commit_hooks("c", &parse("c", &config).unwrap()).unwrap()
}

#[test]
fn ci_structure_rules() {
    let workflow = jobs(
        &parse(
            "ci.yaml",
            "jobs:
  pre-commit-all:
    steps:
      - env:
          SKIP: cargo-test,cargo-clippy,unknown-hook
  rust:
    steps: []
  test:
    steps: []
  markdownlint:
    steps: []
  ci-result:
    needs: [pre-commit-all, test]
",
        )
        .unwrap(),
    );
    let hooks = hooks(&["cargo-test", "cargo-clippy"]);

    // rust is a gate ci-result does not need; markdownlint only annotates.
    assert_eq!(
        crate::strings(&GATES.result_problems(&workflow)),
        ["ci-result does not need the rust job"]
    );
    // cargo-test is run by rust, which ci-result does not need; cargo-clippy is run by no
    // dedicated job; unknown-hook is not a hook at all.
    assert_eq!(
        crate::strings(&GATES.skip_problems(&workflow, &hooks)),
        [
            "pre-commit-all skips cargo-test, run by the rust job, which ci-result does not need",
            "pre-commit-all skips cargo-clippy, which no dedicated job is recorded to run",
            "pre-commit-all skips unknown-hook, which is no hook",
        ]
    );
    let fixed = jobs(
        &parse(
            "ci.yaml",
            "jobs:\n  pre-commit-all:\n    steps:\n      - env:\n          SKIP: cargo-test\n  \
             rust:\n    steps: []\n  ci-result:\n    needs: [pre-commit-all, rust]\n",
        )
        .unwrap(),
    );
    assert_eq!(GATES.result_problems(&fixed), []);
    assert_eq!(GATES.skip_problems(&fixed, &hooks), []);

    for (workflow, want) in [
        (
            "jobs:\n  ci-result:\n    needs: []\n",
            "the CI workflow has no pre-commit-all job",
        ),
        (
            "jobs:\n  pre-commit-all:\n    steps: []\n  ci-result:\n    needs: [pre-commit-all]\n",
            "pre-commit-all skips nothing; the check would pass on nothing",
        ),
    ] {
        assert_eq!(
            crate::strings(
                &GATES.skip_problems(&jobs(&parse("ci.yaml", workflow).unwrap()), &hooks)
            ),
            [want]
        );
    }
    assert_eq!(
        crate::strings(&GATES.result_problems(&jobs(
            &parse("ci.yaml", "jobs:\n  test:\n    steps: []\n").unwrap()
        ))),
        ["the CI workflow has no ci-result job"]
    );
}

/// A workflow with nothing but the aggregator is a problem, not a pass on nothing.
#[test]
fn empty_inputs_are_problems() {
    let gates = CiGates {
        result_job: "ci-result",
        pre_commit_job: "pre-commit-all",
        annotate_only: &["actionlint"],
        skipped_hook_runners: &[],
    };
    let document = parse(
        "ci.yaml",
        "jobs:\n  ci-result:\n    steps: []\n  actionlint:\n    steps: []\n",
    )
    .unwrap();
    assert_eq!(
        strings(&gates.result_problems(&jobs(&document))),
        ["the CI workflow has no gate job beside ci-result; the check would pass on nothing"]
    );
}
