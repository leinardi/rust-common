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

//! The shipped image's invariants, read from the files with [`crate::dockerfile`] so no tool is
//! needed: one Dockerfile, a frontend pinned by digest, the stages from pinned bases, the build
//! stage (the build platform, the build image, what it must do, and every download checked), the
//! version stamps from the Dockerfile and make alike, the runtime posture and its labels, the
//! platforms the workflows declare, and the labels `droast.toml` requires.
//!
//! Each rule takes its project's values: the stage names, the base repositories, the instruction
//! texts, the stamp prefix, the platforms and the labels.
//!
//! Extracted from swarm-device-access/meta/tests/dockerfiles.rs at 8b4ab3a3e417 (the rules as
//! functions of the contents, the walk for Dockerfiles, `build_stage_problems`, `stamp_problems`,
//! `runtime_problems`, `platform_problems`, `label_policy_problems`) with
//! swarm-scheduler-exporter/meta/tests/dockerfiles.rs at c9e79afa8810 (each stamp from its make
//! variable, and the prefix as a whole line of the Makefile) and bitgauge/meta/tests/dockerfiles.rs
//! at 600fc3905200 (every base pinned as its registry requires).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use toml::Table;

use crate::dockerfile::{self, Stage};
use crate::{Problem, Repo, regex};

fn is_dockerfile(name: &str) -> bool {
    name == "Dockerfile" || name.ends_with(".Dockerfile")
}

/// Every Dockerfile (`Dockerfile` or `*.Dockerfile`) under `repo`, sorted, skipping installed and
/// generated trees: hidden directories (`.git`, tool caches, a nested worktree), `dist`, `vendor`
/// and `target`. Walking rather than asking git finds an untracked one too.
///
/// # Errors
///
/// A directory cannot be read.
pub fn dockerfiles(repo: &Repo) -> Result<Vec<String>, Problem> {
    let mut found = Vec::new();
    let mut pending = vec![String::new()];
    while let Some(directory) = pending.pop() {
        let path = if directory.is_empty() {
            repo.root().to_owned()
        } else {
            repo.path(&directory)
        };
        let entries = fs::read_dir(&path)
            .map_err(|error| Problem::at(&directory, format!("cannot be listed: {error}")))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| Problem::at(&directory, format!("cannot be listed: {error}")))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if directory.is_empty() {
                name.clone()
            } else {
                format!("{directory}/{name}")
            };
            let kind = entry
                .file_type()
                .map_err(|error| Problem::at(&relative, format!("cannot be inspected: {error}")))?;
            if kind.is_dir() {
                if !name.starts_with('.') && !matches!(name.as_str(), "dist" | "vendor" | "target")
                {
                    pending.push(relative);
                }
            } else if is_dockerfile(&name) {
                found.push(relative);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// What is wrong unless `found` (from [`dockerfiles`]) is exactly `shipped`: a second Dockerfile
/// would look shipped to every reader and be built by nothing.
#[must_use]
pub fn only_dockerfile_problems(found: &[String], shipped: &str) -> Vec<Problem> {
    if found == [shipped] {
        Vec::new()
    } else {
        vec![Problem::new(format!(
            "the repository holds the Dockerfiles {found:?}; want only {shipped}"
        ))]
    }
}

/// What is wrong with the syntax directive of `content` (the text of `file`): it must name
/// `docker/dockerfile`, by tag and digest, so the frontend cannot be moved under a build.
#[must_use]
pub fn syntax_problems(file: &str, content: &str) -> Vec<Problem> {
    match dockerfile::syntax(content) {
        None => vec![Problem::at(
            file,
            "has no syntax parser directive on its first lines",
        )],
        Some(syntax) if dockerfile::repository(&syntax) != "docker/dockerfile" => {
            vec![Problem::at(
                file,
                format!("the frontend {syntax:?} is not docker/dockerfile"),
            )]
        }
        Some(syntax) if !dockerfile::digest_pinned(&syntax) => vec![Problem::at(
            file,
            format!("the frontend {syntax:?} is not pinned by tag and digest"),
        )],
        Some(_) => Vec::new(),
    }
}

/// What is wrong with the stages of `content` (the text of `file`): they must be exactly `names`,
/// in order, each from a base pinned as its registry requires ([`dockerfile::base_pinned`]).
#[must_use]
pub fn stage_problems(file: &str, content: &str, names: &[&str]) -> Vec<Problem> {
    let stages = match dockerfile::parse(content) {
        Ok(stages) => stages,
        Err(error) => return vec![Problem::at(file, error.to_string())],
    };
    let mut problems = Vec::new();
    let found: Vec<&str> = stages.iter().map(|stage| stage.name.as_str()).collect();
    if found != names {
        problems.push(Problem::at(
            file,
            format!("has the stages {found:?}, want {names:?}"),
        ));
    }
    for stage in &stages {
        if !dockerfile::base_pinned(&stage.image) {
            problems.push(Problem::at(
                file,
                format!(
                    "stage {:?} is FROM {:?}, which is not pinned as its registry requires \
                     (dhi.io by an explicit tag alone, any other by tag and digest)",
                    stage.name, stage.image
                ),
            ));
        }
    }
    problems
}

/// A build stage's rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildStage<'rules> {
    /// The `--platform` it must run on, such as `$BUILDPLATFORM`.
    pub platform: &'rules str,
    /// The repository of its base, such as `dhi.io/rust`.
    pub repository: &'rules str,
    /// Texts its joined instructions must contain: the build arguments, the triples, the cargo
    /// command with `--locked`, the copy of the binary.
    pub required: &'rules [&'rules str],
}

/// What is wrong with a build stage against `rules`, and unless every `curl -fsSL` download in it
/// is checked by a `| sha256sum -c -` of its own, and it downloads something.
#[must_use]
pub fn build_stage_problems(build: &Stage, rules: &BuildStage<'_>) -> Vec<Problem> {
    let mut problems = Vec::new();
    if build.platform != rules.platform {
        problems.push(Problem::new(format!(
            "the build stage runs on --platform={:?}, want {}",
            build.platform, rules.platform
        )));
    }
    if dockerfile::repository(&build.image) != rules.repository {
        problems.push(Problem::new(format!(
            "the build stage is FROM {:?}, want {}",
            build.image, rules.repository
        )));
    }
    let instructions = build.joined();
    for want in rules.required {
        if !instructions.contains(want) {
            problems.push(Problem::new(format!("the build stage lacks {want:?}")));
        }
    }
    let downloads = instructions.matches("curl -fsSL").count();
    let checks = instructions.matches("| sha256sum -c -").count();
    if downloads == 0 {
        problems.push(Problem::new("the build stage downloads nothing"));
    }
    if downloads != checks {
        problems.push(Problem::new(format!(
            "the build stage downloads {downloads} files and checks {checks}"
        )));
    }
    problems
}

/// The version stamps: the variables `<prefix>_<field>` a version module reads at compile time,
/// set by the image's build stage from its build arguments and by make-common's `rust.mk` from
/// its git values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamps<'stamps> {
    /// The prefix, such as `SWARM_DEVICE_ACCESS`; the Makefile sets `RUST_STAMP_PREFIX` to it.
    pub prefix: &'stamps str,
    /// Each field and the make variable `rust.mk` stamps it from, such as
    /// `("VERSION", "GIT_VERSION")`; the build stage stamps it from `${<field>}`.
    pub fields: &'stamps [(&'stamps str, &'stamps str)],
    /// The version module, such as `src/version.rs`, named in a problem.
    pub version_file: &'stamps str,
}

/// make-common's stamp fields.
pub const STAMP_FIELDS: [(&str, &str); 3] = [
    ("VERSION", "GIT_VERSION"),
    ("COMMIT", "GIT_COMMIT"),
    ("DATE", "BUILD_DATE"),
];

impl Stamps<'_> {
    /// What is wrong with the stamps: `version_rs` must read each variable with `option_env!`,
    /// `build` (the build stage's joined instructions) must set each from its build argument,
    /// `makefile` must set `RUST_STAMP_PREFIX := <prefix>` on a line of its own, and `fragment`
    /// (make-common's `rust.mk`) must stamp each `$(RUST_STAMP_PREFIX)_<field>` from its make
    /// variable and start the `rust-build` recipe with `$(RUST_VERSION_ENV)`. A path that stamped
    /// nothing would ship a binary reporting its unstamped default.
    #[must_use]
    pub fn problems(
        &self,
        version_rs: &str,
        build: &str,
        makefile: &str,
        fragment: &str,
    ) -> Vec<Problem> {
        let mut problems = Vec::new();
        let prefix_line = format!("RUST_STAMP_PREFIX := {}", self.prefix);
        if !makefile.lines().any(|line| line.trim_end() == prefix_line) {
            problems.push(Problem::new(format!(
                "the Makefile does not set {prefix_line} on a line of its own"
            )));
        }
        let stamped_build = fragment
            .split("\nrust-build:")
            .nth(1)
            .and_then(|rest| rest.lines().nth(1))
            .is_some_and(|recipe| recipe.starts_with("\t$(RUST_VERSION_ENV) "));
        if !stamped_build {
            problems.push(Problem::new(
                "the rust-build recipe does not start with $(RUST_VERSION_ENV)",
            ));
        }
        for (field, source) in self.fields {
            let variable = format!("{}_{field}", self.prefix);
            if !version_rs.contains(&format!("option_env!(\x22{variable}\x22)")) {
                problems.push(Problem::new(format!(
                    "{} does not read {variable}",
                    self.version_file
                )));
            }
            if !build.contains(&format!("{variable}=\x22${{{field}}}\x22")) {
                problems.push(Problem::new(format!(
                    "the build stage does not stamp {variable} from ${field}"
                )));
            }
            if !fragment.contains(&format!("$(RUST_STAMP_PREFIX)_{field}=\x22$({source})\x22")) {
                problems.push(Problem::new(format!(
                    "rust.mk does not stamp $(RUST_STAMP_PREFIX)_{field} from $({source})"
                )));
            }
        }
        problems
    }
}

/// A runtime stage's rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeStage<'rules> {
    /// The repository of its base, such as `dhi.io/static`.
    pub repository: &'rules str,
    /// Keywords it may not carry: `RUN`, `SHELL`, `VOLUME`, `WORKDIR`, `ADD`, `ENV`, and where the
    /// image has none, `HEALTHCHECK` and `EXPOSE`.
    pub forbidden: &'rules [&'rules str],
    /// Keywords it carries exactly once with exactly these arguments: `USER`, `ENTRYPOINT`, the
    /// `COPY` of the binary.
    pub exactly: &'rules [(&'rules str, &'rules str)],
}

/// What is wrong with a runtime stage against `rules`, which also follows the target platform
/// (no `--platform`).
#[must_use]
pub fn runtime_problems(runtime: &Stage, rules: &RuntimeStage<'_>) -> Vec<Problem> {
    let mut problems = Vec::new();
    if dockerfile::repository(&runtime.image) != rules.repository {
        problems.push(Problem::new(format!(
            "the runtime stage is FROM {:?}, want {}",
            runtime.image, rules.repository
        )));
    }
    if !runtime.platform.is_empty() {
        problems.push(Problem::new(format!(
            "the runtime stage pins --platform={}; it has to follow the target platform",
            runtime.platform
        )));
    }
    let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for instruction in &runtime.instructions {
        if rules.forbidden.contains(&instruction.keyword.as_str()) {
            problems.push(Problem::new(format!(
                "the runtime stage carries {} {}",
                instruction.keyword, instruction.args
            )));
        }
        seen.entry(instruction.keyword.as_str())
            .or_default()
            .push(instruction.args.as_str());
    }
    for (keyword, args) in rules.exactly {
        if seen.get(keyword).map(Vec::as_slice) != Some([*args].as_slice()) {
            problems.push(Problem::new(format!(
                "the runtime stage's {keyword} is {:?}, want exactly {keyword} {args}",
                seen.get(keyword)
            )));
        }
    }
    problems
}

/// What is wrong unless the labels of `stage` are exactly `want`: each key with its value, or
/// with any value that is not empty (`None`, such as a description).
#[must_use]
pub fn label_problems(stage: &Stage, want: &[(&str, Option<&str>)]) -> Vec<Problem> {
    let labels = dockerfile::labels(stage);
    let mut problems = Vec::new();
    for (key, value) in want {
        match (labels.get(*key), value) {
            (Some(found), None) if !found.is_empty() => {}
            (Some(found), Some(value)) if found == value => {}
            (found, value) => problems.push(Problem::new(format!(
                "the {} stage's label {key} is {found:?}, want {}",
                stage.name,
                value.map_or_else(|| "a value".to_owned(), |value| format!("{value:?}"))
            ))),
        }
    }
    for key in labels.keys() {
        if !want.iter().any(|(wanted, _)| wanted == key) {
            problems.push(Problem::new(format!(
                "the {} stage carries the label {key}, which is not one of the wanted labels",
                stage.name
            )));
        }
    }
    problems
}

/// A `platforms:` or `PLATFORMS:` declaration in a workflow, with its value.
static PLATFORM_DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?mi)^\s*(?:-\s*)?(platforms?|PLATFORMS)\s*:\s*(.+)$"));

/// What is wrong with the platforms `workflows` (path and text) declare: each literal
/// declaration is exactly `want` (an expression is not a declaration), none of `forbidden` is
/// named anywhere, and at least one declaration exists, or the check would pass on nothing.
#[must_use]
pub fn platform_problems(
    workflows: &[(String, String)],
    want: &str,
    forbidden: &[&str],
) -> Vec<Problem> {
    let mut declared = 0;
    let mut problems = Vec::new();
    for (workflow, content) in workflows {
        for captures in PLATFORM_DECLARATION.captures_iter(content) {
            let value = captures[2].trim().trim_matches(['"', '\'']);
            if value.starts_with("${{") {
                continue;
            }
            declared += 1;
            if value != want {
                problems.push(Problem::at(
                    workflow,
                    format!("declares {} {value:?}, want {want:?}", &captures[1]),
                ));
            }
        }
        for platform in forbidden {
            if content.contains(platform) {
                problems.push(Problem::at(
                    workflow,
                    format!("names {platform}, a platform other than {want}"),
                ));
            }
        }
    }
    if declared == 0 {
        problems.push(Problem::new(
            "no workflow declares its platforms; the check would pass on nothing",
        ));
    }
    problems
}

/// What is wrong with the label policy: `droast` (the text of `droast_file`) must refuse any label
/// outside its `[required-labels]` (`strict-labels = true`), and require exactly the labels the
/// last stage of `dockerfile_text` (the text of `dockerfile_file`) carries.
#[must_use]
pub fn label_policy_problems(
    dockerfile_file: &str,
    dockerfile_text: &str,
    droast_file: &str,
    droast: &str,
) -> Vec<Problem> {
    let config: Table = match droast.parse() {
        Ok(config) => config,
        Err(error) => return vec![Problem::at(droast_file, format!("parse: {error}"))],
    };
    let mut problems = Vec::new();
    if config.get("strict-labels").and_then(toml::Value::as_bool) != Some(true) {
        problems.push(Problem::at(
            droast_file,
            "does not set strict-labels = true, so droast accepts any label",
        ));
    }
    let required: BTreeSet<String> = config
        .get("required-labels")
        .and_then(toml::Value::as_table)
        .map(|labels| labels.keys().cloned().collect())
        .unwrap_or_default();
    if required.is_empty() {
        problems.push(Problem::at(droast_file, "requires no label"));
    }
    let carried: BTreeSet<String> = match dockerfile::parse(dockerfile_text) {
        Ok(stages) => stages
            .last()
            .map(|runtime| dockerfile::labels(runtime).into_keys().collect())
            .unwrap_or_default(),
        Err(error) => return vec![Problem::at(dockerfile_file, error.to_string())],
    };
    for label in required.difference(&carried) {
        problems.push(Problem::at(
            droast_file,
            format!("requires {label}, which the runtime stage does not carry"),
        ));
    }
    for label in carried.difference(&required) {
        problems.push(Problem::at(
            dockerfile_file,
            format!("the runtime stage carries {label}, which {droast_file} does not require"),
        ));
    }
    problems
}

/// Whether the last component of `path` names a Dockerfile: `Dockerfile` or `*.Dockerfile`.
#[must_use]
pub fn is_dockerfile_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_dockerfile)
}

#[cfg(test)]
mod tests;
