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

//! The shipped image's pins, read from the files with [`crate::dockerfile`] so no tool is needed:
//! the Dockerfiles a repository holds, a frontend pinned by digest, and the stages from bases
//! pinned as the project's [`BasePolicy`] requires.
//!
//! Each rule takes its project's values: the stage names and the base policy.
//!
//! What stays in a consumer's own meta-tests: swarm-rootless-docker's exact dind image, its rules
//! that the last USER is the unprivileged one, that the user rewrite is the stage's one RUN and
//! that nothing is installed with apk, and its `base.name` label; awtrix-controller's and
//! swarm-rootless-docker's check that the release workflow supplies every label argument
//! (`label_arguments_are_supplied`); and every check that runs a tool, such as droast.
//!
//! Extracted from swarm-device-access/meta/tests/dockerfiles.rs at 8b4ab3a3e417 and
//! `bitgauge/meta/tests/dockerfiles.rs` at 600fc3905200 (the Dockerfile names and their listing,
//! every base from dhi.io).

use std::path::Path;

use crate::dockerfile;
use crate::{Problem, Repo};

/// Whether a file name is a Dockerfile: `Dockerfile`, `<name>.Dockerfile` or `Dockerfile.<name>`,
/// but not the `Dockerfile.<name>.dockerignore` `BuildKit` reads beside one.
fn is_dockerfile(name: &str) -> bool {
    name == "Dockerfile"
        || name.ends_with(".Dockerfile")
        || (name.starts_with("Dockerfile.") && !name.ends_with(".dockerignore"))
}

/// Every Dockerfile under `repo` that is tracked or new ([`Repo::tracked_or_new`]), sorted: git
/// keeps the ignored trees (`target/`, `node_modules/`, `dist/`) out without naming them, and a
/// hidden directory is searched like any other.
///
/// # Panics
///
/// git cannot list the files.
#[must_use]
pub fn dockerfiles(repo: &Repo) -> Vec<String> {
    repo.tracked_or_new(&["*Dockerfile*"])
        .into_iter()
        .filter(|path| is_dockerfile_path(path))
        .collect()
}

/// What is wrong with the syntax directive of `content` (the text of `file`): it must name
/// `docker/dockerfile`, by tag and digest, so the frontend cannot be moved under a build. With
/// `required` false a file may have none (`BuildKit`'s own frontend), but one it has is held to
/// the same rule.
#[must_use]
pub fn syntax_problems(file: &str, content: &str, required: bool) -> Vec<Problem> {
    match dockerfile::syntax(content) {
        None if required => vec![Problem::at(
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
        None | Some(_) => Vec::new(),
    }
}

/// How a project pins its stages' bases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BasePolicy {
    /// A dhi.io base by an explicit tag alone, any other by tag and digest
    /// ([`dockerfile::base_pinned`]; swarm-scheduler-exporter, swarm-device-access,
    /// awtrix-controller).
    DhiTagElseDigest,
    /// Every base from dhi.io, by an explicit tag alone (bitgauge).
    DhiOnly,
    /// Every base by an explicit tag alone, from any registry, such as
    /// `docker:29-dind-rootless` (swarm-rootless-docker, whose base moves with its tag on
    /// purpose).
    TagOnly,
}

impl BasePolicy {
    fn allows(self, image: &str) -> bool {
        match self {
            Self::DhiTagElseDigest => dockerfile::base_pinned(image),
            Self::DhiOnly => dockerfile::from_dhi(image) && dockerfile::tag_pinned(image),
            Self::TagOnly => dockerfile::tag_pinned(image),
        }
    }

    fn rule(self) -> &'static str {
        match self {
            Self::DhiTagElseDigest => {
                "dhi.io by an explicit tag alone, any other by tag and digest"
            }
            Self::DhiOnly => "a literal dhi.io reference by an explicit tag alone",
            Self::TagOnly => "a literal reference by an explicit tag alone",
        }
    }
}

/// What is wrong with the stages of `content` (the text of `file`): they must be exactly `names`,
/// in order, each from a base `policy` allows; and `names` must name one, or the check would pass
/// on nothing.
#[must_use]
pub fn stage_problems(
    file: &str,
    content: &str,
    names: &[&str],
    policy: BasePolicy,
) -> Vec<Problem> {
    if names.is_empty() {
        return vec![Problem::at(
            file,
            "no stage is wanted; the check would pass on nothing",
        )];
    }
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
        if !policy.allows(&stage.image) {
            problems.push(Problem::at(
                file,
                format!(
                    "stage {:?} is FROM {:?}, which is not pinned as the project requires ({})",
                    stage.name,
                    stage.image,
                    policy.rule()
                ),
            ));
        }
    }
    problems
}

/// Whether the last component of `path` names a Dockerfile: `Dockerfile`, `<name>.Dockerfile` or
/// `Dockerfile.<name>`, but not `Dockerfile.<name>.dockerignore`.
#[must_use]
pub fn is_dockerfile_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_dockerfile)
}

#[cfg(test)]
mod tests;
