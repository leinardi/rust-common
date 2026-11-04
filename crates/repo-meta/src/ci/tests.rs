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
//! (`ci_structure_rules`, `weakened_rustdoc_gates_are_refused`),
//! swarm-scheduler-exporter/meta/tests/toolchain.rs at c9e79afa8810 (the narrowed features and the
//! skipped or tolerated step) and swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417
//! (`always_run_rules`).

use std::fmt::Write as _;

use super::{CiGates, MAKE_COMMON_RUSTDOC, always_run_problems};
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

const FRAGMENT: &str = "RUST_FEATURES ?= --all-features\n\n\
rust-doc: ## Check the Rust documentation, warnings as errors\n\
\tRUSTDOCFLAGS=\"$(RUSTDOCFLAGS) -D warnings\" $(CARGO) doc --workspace $(RUST_FEATURES) --no-deps --locked\n\n\
verify-rust: rust-build rust-fmt-check rust-clippy rust-test rust-doc ## Verify\n";

const WORKFLOW: &str =
    "jobs:\n  rust:\n    steps:\n      - name: Docs\n        run: make rust-doc\n";

#[test]
fn rustdoc_gate_rules() {
    let check = |fragment: &str, makefile: &str, workflow: &str| {
        MAKE_COMMON_RUSTDOC.problems(fragment, makefile, &parse("ci.yaml", workflow).unwrap())
    };
    let makefile = "# RUST_FEATURES=--all-features is rust.mk's default\nBIN_NAME ?= x\n";
    assert_eq!(check(FRAGMENT, makefile, WORKFLOW), []);
    for (name, fragment) in [
        (
            "not in verify",
            FRAGMENT.replace("rust-test rust-doc", "rust-test"),
        ),
        ("warnings allowed", FRAGMENT.replace(" -D warnings", "")),
        (
            "fewer features",
            FRAGMENT.replace("RUST_FEATURES ?= --all-features", "RUST_FEATURES ?="),
        ),
    ] {
        assert_ne!(fragment, FRAGMENT, "{name} changed nothing");
        assert_eq!(check(&fragment, makefile, WORKFLOW).len(), 1, "{name}");
    }
    assert_eq!(
        check(
            FRAGMENT,
            &format!("{makefile}RUST_FEATURES := --features x\n"),
            WORKFLOW
        )
        .len(),
        1,
        "a narrowed Makefile"
    );
    for (name, workflow) in [
        (
            "no CI step",
            WORKFLOW.replace("run: make rust-doc", "run: echo missing documentation gate"),
        ),
        (
            "a skipped step",
            WORKFLOW.replace(
                "run: make rust-doc",
                "if: false\n        run: make rust-doc",
            ),
        ),
        (
            "a tolerated step",
            WORKFLOW.replace(
                "run: make rust-doc",
                "continue-on-error: true\n        run: make rust-doc",
            ),
        ),
    ] {
        assert_eq!(check(FRAGMENT, makefile, &workflow).len(), 1, "{name}");
    }
}

/// A whole-tree hook that is missing, doubled, passed file names, not always run, or eligible at
/// another stage is refused.
#[test]
fn always_run_rules() {
    let hook = |id: &str, settings: &str| {
        format!("      - id: {id}\n        entry: x\n        language: system\n{settings}")
    };
    let good = "        pass_filenames: false\n        always_run: true\n        stages: \
                [pre-commit]\n";
    let config = |docs_links: &str| {
        let text = format!(
            "repos:\n  - repo: local\n    hooks:\n{}{}{docs_links}",
            hook("check-license-headers", good),
            hook("check-suppressions", good),
        );
        pre_commit_hooks("c", &parse("c", &text).unwrap()).unwrap()
    };
    let ids = ["check-license-headers", "check-suppressions", "docs-links"];
    assert_eq!(
        always_run_problems(&config(&hook("docs-links", good)), &ids),
        []
    );
    for (name, docs_links) in [
        ("a missing hook", String::new()),
        (
            "a doubled hook",
            format!("{}{}", hook("docs-links", good), hook("docs-links", good)),
        ),
        (
            "file names passed",
            hook(
                "docs-links",
                &good.replace("pass_filenames: false", "pass_filenames: true"),
            ),
        ),
        (
            "not always run",
            hook(
                "docs-links",
                &good.replace("always_run: true", "always_run: false"),
            ),
        ),
        (
            "every stage",
            hook(
                "docs-links",
                &good.replace("stages: [pre-commit]", "stages: [pre-commit, commit-msg]"),
            ),
        ),
    ] {
        assert_eq!(
            always_run_problems(&config(&docs_links), &ids).len(),
            1,
            "{name} was not refused once"
        );
    }
}
