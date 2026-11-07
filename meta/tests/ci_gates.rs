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

//! The CI gates hold together: `ci-result`, the one required check, needs every gating job; every
//! hook `pre-commit-all` skips exists and is run by a job `ci-result` needs; the whole-tree hooks
//! always run, take no file list and run at the pre-commit stage only; and the rustdoc gate is in
//! `verify-rust`, fails on warnings over every feature, and runs unconditionally in the `rust`
//! job. The checks are `repo_meta::ci`'s; the negative controls run them on altered copies of this
//! repository's own files.
//!
//! Extracted from swarm-device-access/meta/tests/ci_gates.rs at 8b4ab3a3e417 and
//! swarm-scheduler-exporter/meta/tests/toolchain.rs at c9e79afa8810 (the rustdoc gate).

use repo_meta::ci::{CiGates, MAKE_COMMON_RUSTDOC, always_run_problems};
use repo_meta::{Problem, report, strings, yaml};

const WORKFLOW: &str = ".github/workflows/ci.yaml";
const HOOKS: &str = ".pre-commit-config.yaml";
const RUST_FRAGMENT: &str = ".mk/rust.mk";

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

/// The hooks that read the whole tree rather than the files a commit touches.
const WHOLE_TREE_HOOKS: &[&str] = &["check-license-headers"];

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

#[test]
fn the_whole_tree_hooks_always_run() {
    let problems = always_run_problems(&hooks(&meta::repo().read(HOOKS)), WHOLE_TREE_HOOKS);
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn the_rustdoc_gate_fails_on_warnings_in_ci() {
    let repo = meta::repo();
    let problems = MAKE_COMMON_RUSTDOC.problems(
        &repo.read(RUST_FRAGMENT),
        &repo.read("Makefile"),
        &parse(WORKFLOW, &repo.read(WORKFLOW)),
    );
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

/// Negative control: a whole-tree hook that takes the file list, runs only on matching files, or
/// runs at every installed stage.
#[test]
fn always_run_check_is_not_blind() {
    let config = meta::repo().read(HOOKS);
    let entry = "        entry: scripts/check-license-headers.sh\n        language: system\n";
    let settings = [
        "        pass_filenames: false\n",
        "        always_run: true\n",
        "        stages: [pre-commit]\n",
    ];
    let licence = format!("{entry}{}", settings.concat());
    for (dropped, want) in [(0, "pass_filenames"), (1, "always_run"), (2, "stage")] {
        let mut kept = settings.to_vec();
        kept.remove(dropped);
        let change = format!("{entry}{}", kept.concat());
        let problems = always_run_problems(
            &hooks(&altered(&config, &licence, &change)),
            WHOLE_TREE_HOOKS,
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].message.contains("check-license-headers")
                && problems[0].message.contains(want),
            "{problems:?}"
        );
    }
    let problems = always_run_problems(&hooks(&config), &["no-such-hook"]);
    assert_eq!(
        strings(&problems),
        ["hook no-such-hook appears 0 times, want once"]
    );
}

/// Negative control: a rustdoc recipe without `-D warnings`, a Makefile that narrows the features,
/// and a CI step allowed to fail.
#[test]
fn rustdoc_gate_check_is_not_blind() {
    let repo = meta::repo();
    let fragment = repo.read(RUST_FRAGMENT);
    let makefile = repo.read("Makefile");
    let workflow = repo.read(WORKFLOW);
    let problems = |fragment: &str, makefile: &str, workflow: &str| {
        MAKE_COMMON_RUSTDOC.problems(fragment, makefile, &parse(WORKFLOW, workflow))
    };

    let lenient = altered(&fragment, "$(RUSTDOCFLAGS) -D warnings", "$(RUSTDOCFLAGS)");
    assert_eq!(problems(&lenient, &makefile, &workflow).len(), 1);

    let narrowed = format!("{makefile}RUST_FEATURES := --features x\n");
    assert_eq!(
        strings(&problems(&fragment, &narrowed, &workflow)),
        ["the Makefile sets RUST_FEATURES, narrowing the documented features"]
    );

    let allowed_to_fail = altered(
        &workflow,
        "        run: make rust-doc\n",
        "        run: make rust-doc\n        continue-on-error: true\n",
    );
    assert_eq!(problems(&fragment, &makefile, &allowed_to_fail).len(), 1);
}
