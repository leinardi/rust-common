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

//! The YAML the checks read: workflows, composite actions and the pre-commit configuration,
//! parsed with serde-saphyr into `serde_json`'s [`Value`], and the parts of them the checks need:
//! the steps that use an action ([`action_steps`]), the jobs of a workflow ([`jobs`]) and the
//! hooks of a pre-commit configuration ([`pre_commit_hooks`]).
//!
//! Extracted from bitgauge/meta/tests/toolchain.rs at 600fc3905200 (`parse_yaml`, `scalar_text`,
//! `action_steps` with the `uses:` reference), swarm-device-access/meta/tests/ci_gates.rs at
//! 8b4ab3a3e417 (`parse_ci`) and swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417
//! (`pre_commit_hooks`).

use std::collections::BTreeMap;

pub use serde_json::Value;

use crate::Problem;

/// Parses the YAML document `content` of `file`.
///
/// # Errors
///
/// It is not YAML: a problem at `file`.
pub fn parse(file: &str, content: &str) -> Result<Value, Problem> {
    serde_saphyr::from_str(content).map_err(|error| Problem::at(file, format!("parse: {error}")))
}

/// A YAML scalar as text, the way Go's `fmt.Sprint` writes it: a string as it is, a number or a
/// boolean in its plain form, null as `<nil>`, anything else as JSON.
#[must_use]
pub fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "<nil>".to_owned(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

/// One mapping with a `uses:` key: a job step, a composite action step or a reusable workflow
/// call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionStep {
    /// The file it is in.
    pub file: String,
    /// The `uses:` value before its `@`.
    pub action: String,
    /// The `uses:` value after its `@`, empty when it has none.
    pub reference: String,
    /// Its `with:` inputs, as [`scalar_text`].
    pub with: BTreeMap<String, String>,
}

/// Every mapping with a `uses:` key anywhere in `document`, in document order.
#[must_use]
pub fn action_steps(file: &str, document: &Value) -> Vec<ActionStep> {
    let mut steps = Vec::new();
    walk_steps(file, document, &mut steps);
    steps
}

fn walk_steps(file: &str, node: &Value, steps: &mut Vec<ActionStep>) {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(uses)) = map.get("uses") {
                let (action, reference) = uses.split_once('@').unwrap_or((uses, ""));
                let with = match map.get("with") {
                    Some(Value::Object(inputs)) => inputs
                        .iter()
                        .map(|(key, input)| (key.clone(), scalar_text(input)))
                        .collect(),
                    _ => BTreeMap::new(),
                };
                steps.push(ActionStep {
                    file: file.to_owned(),
                    action: action.to_owned(),
                    reference: reference.to_owned(),
                    with,
                });
            }
            for value in map.values() {
                walk_steps(file, value, steps);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_steps(file, item, steps);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// One job of a workflow: the jobs it needs, and its steps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Job {
    /// Its `needs:`, a string or a list.
    pub needs: Vec<String>,
    /// Its `steps:`, each as written.
    pub steps: Vec<Value>,
}

impl Job {
    /// The `SKIP` value of each step's `env:` that sets one.
    #[must_use]
    pub fn skips(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter_map(|step| step.get("env")?.get("SKIP")?.as_str())
            .map(str::to_owned)
            .collect()
    }
}

/// The jobs of a workflow document, by name; empty when it has none.
#[must_use]
pub fn jobs(document: &Value) -> BTreeMap<String, Job> {
    let Some(Value::Object(jobs)) = document.get("jobs") else {
        return BTreeMap::new();
    };
    jobs.iter()
        .map(|(name, job)| {
            let needs = match job.get("needs") {
                Some(Value::String(need)) => vec![need.clone()],
                Some(Value::Array(needs)) => needs
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            };
            let steps = job
                .get("steps")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            (name.clone(), Job { needs, steps })
        })
        .collect()
}

/// The hooks of a pre-commit configuration document, by id, in file order; an id with aliases
/// appears once per entry.
///
/// # Errors
///
/// The document has no `repos:` list: a problem at `file`.
pub fn pre_commit_hooks(file: &str, document: &Value) -> Result<Vec<(String, Value)>, Problem> {
    let repos = document
        .get("repos")
        .and_then(Value::as_array)
        .ok_or_else(|| Problem::at(file, "has no repos list"))?;
    Ok(repos
        .iter()
        .filter_map(|entry| entry.get("hooks").and_then(Value::as_array))
        .flatten()
        .map(|hook| {
            let id = hook.get("id").and_then(Value::as_str).unwrap_or_default();
            (id.to_owned(), hook.clone())
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{action_steps, jobs, parse, pre_commit_hooks, scalar_text};

    #[test]
    fn steps_jobs_and_hooks() {
        let document = parse(
            "ci.yaml",
            "jobs:\n  a:\n    needs: b\n    steps:\n      - uses: x/y@v1\n        with:\n          \
             toolchain: 1.98\n          flag: true\n      - env:\n          SKIP: one,two\n  b:\n    \
             needs: [c, d]\n    uses: ./.github/workflows/w.yaml\n",
        )
        .unwrap();
        let steps = action_steps("ci.yaml", &document);
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert_eq!(steps[0].action, "x/y");
        assert_eq!(steps[0].reference, "v1");
        assert_eq!(steps[0].with["toolchain"], "1.98");
        assert_eq!(steps[0].with["flag"], "true");
        assert_eq!(steps[1].action, "./.github/workflows/w.yaml");
        assert_eq!(steps[1].reference, "");
        let jobs = jobs(&document);
        assert_eq!(jobs["a"].needs, ["b"]);
        assert_eq!(jobs["a"].skips(), ["one,two"]);
        assert_eq!(jobs["b"].needs, ["c", "d"]);

        let config = parse(
            ".pre-commit-config.yaml",
            "repos:\n  - repo: local\n    hooks:\n      - id: a\n      - id: b\n  - repo: x\n    \
             hooks:\n      - id: a\n",
        )
        .unwrap();
        let hooks = pre_commit_hooks(".pre-commit-config.yaml", &config).unwrap();
        let ids: Vec<&str> = hooks.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "a"]);
        assert!(pre_commit_hooks("x.yaml", &parse("x.yaml", "a: 1\n").unwrap()).is_err());
        assert!(parse("x.yaml", "a: [\n").is_err());
        assert_eq!(scalar_text(&serde_json::Value::Null), "<nil>");
    }
}
