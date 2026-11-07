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

//! The CI gates hold together: `ci-result`, the one required check, needs every gating job, and
//! every hook `pre-commit-all` skips exists and is run by a job `ci-result` needs. The checks are
//! `repo_meta::ci`'s; the negative controls run them on altered copies of this repository's own
//! files.
//!
//! Extracted from swarm-device-access/meta/tests/ci_gates.rs at 8b4ab3a3e417.

use repo_meta::ci::CiGates;
use repo_meta::{Problem, report, strings, yaml};

const WORKFLOW: &str = ".github/workflows/ci.yaml";
const HOOKS: &str = ".pre-commit-config.yaml";

const GATES: CiGates<'static> = CiGates {
    result_job: "ci-result",
    pre_commit_job: "pre-commit-all",
    annotate_only: &[
        "actionlint",
        "pre-commit-hooks",
        "markdownlint",
        "shellcheck",
        "yamllint",
    ],
    skipped_hook_runners: &[("cargo-test", "rust"), ("cargo-deny", "rust")],
};

fn parse(file: &str, content: &str) -> yaml::Value {
    yaml::parse(file, content).unwrap_or_else(|problem| panic!("{problem}"))
}

fn hooks(content: &str) -> Vec<(String, yaml::Value)> {
    yaml::pre_commit_hooks(HOOKS, &parse(HOOKS, content))
        .unwrap_or_else(|problem| panic!("{problem}"))
}

/// The CI gate problems of a workflow and a pre-commit configuration.
fn gate_problems(workflow: &str, config: &str) -> Vec<Problem> {
    let jobs = yaml::jobs(&parse(WORKFLOW, workflow));
    let mut problems = GATES.result_problems(&jobs);
    problems.extend(GATES.skip_problems(&jobs, &hooks(config)));
    problems
}

/// `text` with `from` replaced once, which it must contain.
fn altered(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "the file no longer holds {from:?}");
    text.replacen(from, to, 1)
}

#[test]
fn the_result_job_needs_every_gate_and_every_skip_is_run() {
    let repo = meta::repo();
    let problems = gate_problems(&repo.read(WORKFLOW), &repo.read(HOOKS));
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Negative control: a gating job dropped from `ci-result`'s needs, a skipped hook that does not
/// exist, and a pre-commit job that skips nothing.
#[test]
fn gate_check_is_not_blind() {
    let repo = meta::repo();
    let workflow = repo.read(WORKFLOW);
    let config = repo.read(HOOKS);

    let dropped = altered(&workflow, "      - feature-matrix\n", "");
    assert_eq!(
        strings(&gate_problems(&dropped, &config)),
        ["ci-result does not need the feature-matrix job"]
    );

    let unknown = altered(
        &workflow,
        "SKIP: cargo-test,cargo-deny",
        "SKIP: cargo-test,cargo-deny,cargo-nothing",
    );
    assert_eq!(
        strings(&gate_problems(&unknown, &config)),
        ["pre-commit-all skips cargo-nothing, which is no hook"]
    );

    let unrun = altered(
        &config,
        "      - id: check-merge-conflict\n",
        "      - id: check-merge-conflict\n      - id: unrun\n",
    );
    let unrun_workflow = altered(
        &workflow,
        "SKIP: cargo-test,cargo-deny",
        "SKIP: cargo-test,cargo-deny,unrun",
    );
    assert_eq!(
        strings(&gate_problems(&unrun_workflow, &unrun)),
        ["pre-commit-all skips unrun, which no dedicated job is recorded to run"]
    );

    let nothing = altered(&workflow, "SKIP: cargo-test,cargo-deny", "SKIP: \"\"");
    assert_eq!(
        strings(&gate_problems(&nothing, &config)),
        ["pre-commit-all skips nothing; the check would pass on nothing"]
    );
}
