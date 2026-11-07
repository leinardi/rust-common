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
//! annotation is needed by the aggregator job, the one required check, and the pre-commit job
//! skips only hooks a needed job runs instead.
//!
//! Extracted from swarm-device-access/meta/tests/ci_gates.rs at 8b4ab3a3e417 (the gates and the
//! skips).

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
    /// Every gate job (neither the aggregator nor an annotation) the aggregator does not need; and
    /// a workflow with no gate job beside the aggregator, on which the check would pass.
    #[must_use]
    pub fn result_problems(&self, jobs: &BTreeMap<String, Job>) -> Vec<Problem> {
        let Some(result) = jobs.get(self.result_job) else {
            return vec![Problem::new(format!(
                "the CI workflow has no {} job",
                self.result_job
            ))];
        };
        let gates: Vec<&String> = jobs
            .keys()
            .filter(|name| *name != self.result_job && !self.annotate_only.contains(&name.as_str()))
            .collect();
        if gates.is_empty() {
            return vec![Problem::new(format!(
                "the CI workflow has no gate job beside {}; the check would pass on nothing",
                self.result_job
            ))];
        }
        gates
            .into_iter()
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

#[cfg(test)]
mod tests;
