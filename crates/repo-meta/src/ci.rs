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

//! The CI gates are required: every job of the CI workflow that is a gate rather than an
//! annotation is needed by the aggregator job, the one required check; the pre-commit job skips
//! only hooks a needed job runs instead; the rustdoc gate fails both the local verify target and
//! the required job; and the whole-tree hooks run once, on every commit.
//!
//! Extracted from swarm-device-access/meta/tests/ci_gates.rs at 8b4ab3a3e417 (the gates and the
//! skips, and the rustdoc gate in `verify-rust`, the recipe and the job) with
//! swarm-scheduler-exporter/meta/tests/toolchain.rs at c9e79afa8810 (the rustdoc gate's every
//! feature: `RUST_FEATURES ?= --all-features`, never narrowed by the Makefile; a job step that is
//! not skipped or allowed to fail) and swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417
//! (`always_run_problems`).

use std::collections::BTreeMap;

use crate::Problem;
use crate::yaml::{Job, Value};

/// A CI workflow's gate layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CiGates<'gates> {
    /// The aggregator job branch protection requires, such as `ci-result`.
    pub result_job: &'gates str,
    /// The job that runs every pre-commit hook on every file, such as `pre-commit-all`.
    pub pre_commit_job: &'gates str,
    /// The jobs that only post review comments on a pull request; the aggregator does not need
    /// them, because the pre-commit job runs the same hooks.
    pub annotate_only: &'gates [&'gates str],
    /// For each hook the pre-commit job may skip, the job that runs it instead.
    pub skipped_hook_runners: &'gates [(&'gates str, &'gates str)],
}

impl CiGates<'_> {
    /// Every gate job (neither the aggregator nor an annotation) the aggregator does not need.
    #[must_use]
    pub fn result_problems(&self, jobs: &BTreeMap<String, Job>) -> Vec<Problem> {
        let Some(result) = jobs.get(self.result_job) else {
            return vec![Problem::new(format!(
                "the CI workflow has no {} job",
                self.result_job
            ))];
        };
        jobs.keys()
            .filter(|name| *name != self.result_job && !self.annotate_only.contains(&name.as_str()))
            .filter(|name| !result.needs.contains(name))
            .map(|name| Problem::new(format!("{} does not need the {name} job", self.result_job)))
            .collect()
    }

    /// Every hook the pre-commit job skips that is not one of `hooks` (the pre-commit
    /// configuration's, [`crate::yaml::pre_commit_hooks`]), that no job is recorded to run, or
    /// whose job the aggregator does not need; and a pre-commit job that skips nothing, on which
    /// this check would pass having read nothing.
    #[must_use]
    pub fn skip_problems(
        &self,
        jobs: &BTreeMap<String, Job>,
        hooks: &[(String, Value)],
    ) -> Vec<Problem> {
        let job_name = self.pre_commit_job;
        let Some(job) = jobs.get(job_name) else {
            return vec![Problem::new(format!(
                "the CI workflow has no {job_name} job"
            ))];
        };
        let needed: &[String] = jobs
            .get(self.result_job)
            .map_or(&[], |result| result.needs.as_slice());
        let skipped: Vec<String> = job
            .skips()
            .iter()
            .flat_map(|skip| skip.split(','))
            .map(str::trim)
            .filter(|hook| !hook.is_empty())
            .map(str::to_owned)
            .collect();
        if skipped.is_empty() {
            return vec![Problem::new(format!(
                "{job_name} skips nothing; the check would pass on nothing"
            ))];
        }
        let mut problems = Vec::new();
        for hook in skipped {
            if !hooks.iter().any(|(id, _)| *id == hook) {
                problems.push(Problem::new(format!(
                    "{job_name} skips {hook}, which is no hook"
                )));
                continue;
            }
            match self
                .skipped_hook_runners
                .iter()
                .find(|(skipped, _)| *skipped == hook)
            {
                None => problems.push(Problem::new(format!(
                    "{job_name} skips {hook}, which no dedicated job is recorded to run"
                ))),
                Some((_, runner)) if !needed.iter().any(|need| need == runner) => {
                    problems.push(Problem::new(format!(
                        "{job_name} skips {hook}, run by the {runner} job, which {} does not need",
                        self.result_job
                    )));
                }
                Some(_) => {}
            }
        }
        problems
    }
}

/// Where the rustdoc gate lives: a make target in `verify-rust` with a recipe that turns warnings
/// into errors over every feature, and a step of a required CI job that runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RustdocGate<'gate> {
    /// The verify target that must list the doc target, such as `verify-rust`.
    pub verify_target: &'gate str,
    /// The doc target, such as `rust-doc`.
    pub target: &'gate str,
    /// The doc target's exact rule, its help comment and recipe line included. The match is on
    /// the text, so a reworded recipe fails until this is updated with it: brittle by design,
    /// since a recipe that drops `-D warnings` must not pass.
    pub recipe: &'gate str,
    /// The default the features variable must have in the fragment (every feature), with no
    /// assignment to the variable in the Makefile; `None` where the recipe names its features.
    pub features: Option<(&'gate str, &'gate str)>,
    /// The CI job whose step runs it, such as `rust`.
    pub job: &'gate str,
    /// The step's `run:`, such as `make rust-doc`.
    pub run: &'gate str,
}

/// make-common's `rust.mk` (`leinardi/make-common`), run by the `rust` job's `make rust-doc` step.
pub const MAKE_COMMON_RUSTDOC: RustdocGate<'static> = RustdocGate {
    verify_target: "verify-rust",
    target: "rust-doc",
    recipe: "rust-doc: ## Check the Rust documentation, warnings as errors\n\tRUSTDOCFLAGS=\"$(RUSTDOCFLAGS) -D warnings\" $(CARGO) doc --workspace $(RUST_FEATURES) --no-deps --locked\n",
    features: Some(("RUST_FEATURES", "--all-features")),
    job: "rust",
    run: "make rust-doc",
};

impl RustdocGate<'_> {
    /// What is wrong with the gate: `fragment` (the make snippet that defines the targets),
    /// `makefile` and the CI `workflow` document.
    #[must_use]
    pub fn problems(&self, fragment: &str, makefile: &str, workflow: &Value) -> Vec<Problem> {
        let mut problems = Vec::new();
        let verify_prefix = format!("{}:", self.verify_target);
        let in_verify = fragment.lines().any(|line| {
            line.starts_with(&verify_prefix)
                && line.split_whitespace().any(|target| target == self.target)
        });
        if !in_verify {
            problems.push(Problem::new(format!(
                "{} does not run {}",
                self.verify_target, self.target
            )));
        }
        if !fragment.contains(self.recipe) {
            problems.push(Problem::new(format!(
                "the {} recipe is not the one that fails on warnings: {:?}",
                self.target, self.recipe
            )));
        }
        if let Some((variable, value)) = self.features {
            let default = format!("{variable} ?= {value}");
            if !fragment.lines().any(|line| line == default) {
                problems.push(Problem::new(format!(
                    "the fragment does not default {variable} to {value}"
                )));
            }
            if makefile
                .lines()
                .any(|line| line.trim_start().starts_with(variable))
            {
                problems.push(Problem::new(format!(
                    "the Makefile sets {variable}, narrowing the documented features"
                )));
            }
        }
        let gated = workflow
            .get("jobs")
            .and_then(|jobs| jobs.get(self.job))
            .and_then(|job| job.get("steps"))
            .and_then(Value::as_array)
            .is_some_and(|steps| {
                steps.iter().any(|step| {
                    step.get("run").and_then(Value::as_str) == Some(self.run)
                        && step.get("if").is_none()
                        && step.get("continue-on-error").is_none()
                })
            });
        if !gated {
            problems.push(Problem::new(format!(
                "no step of the {} job runs {:?} unconditionally and without continue-on-error",
                self.job, self.run
            )));
        }
        problems
    }
}

/// What is wrong with the whole-tree hooks `ids` among `hooks` (the pre-commit configuration's):
/// each appears once, takes no file list (`pass_filenames: false`), always runs (a commit that
/// only deletes a file passes it none), and runs at exactly the pre-commit stage (`always_run`
/// makes a hook eligible at every installed stage, commit-msg included).
#[must_use]
pub fn always_run_problems(hooks: &[(String, Value)], ids: &[&str]) -> Vec<Problem> {
    let mut problems = Vec::new();
    for id in ids {
        let found: Vec<&Value> = hooks
            .iter()
            .filter(|(hook_id, _)| hook_id == id)
            .map(|(_, hook)| hook)
            .collect();
        let [hook] = found.as_slice() else {
            problems.push(Problem::new(format!(
                "hook {id} appears {} times, want once",
                found.len()
            )));
            continue;
        };
        if hook.get("always_run").and_then(Value::as_bool) != Some(true) {
            problems.push(Problem::new(format!(
                "hook {id} does not set always_run: true"
            )));
        }
        if hook.get("pass_filenames").and_then(Value::as_bool) != Some(false) {
            problems.push(Problem::new(format!(
                "hook {id} does not set pass_filenames: false"
            )));
        }
        let stages: Option<Vec<&str>> = hook
            .get("stages")
            .and_then(Value::as_array)
            .map(|stages| stages.iter().filter_map(Value::as_str).collect());
        if stages.as_deref() != Some(["pre-commit"].as_slice()) {
            problems.push(Problem::new(format!(
                "hook {id} runs at stages {stages:?}, want exactly [pre-commit]"
            )));
        }
    }
    problems
}

#[cfg(test)]
mod tests;
