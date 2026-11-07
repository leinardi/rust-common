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

//! Scratch git repositories, and the harness that drives a repository's own suppression ratchet
//! (`scripts/check-suppressions.sh`) in them. What is under test is the script's rule, not
//! today's tree, so each case builds a repository of its own; the cases are the caller's, since
//! each repository's suppression rules are its own.
//!
//! Every git command and every script run in a scratch repository goes through
//! [`testenv_core::gitenv::isolate`]: under a commit hook, git exports the location of the
//! repository being committed, and a `git add` in a scratch repository with that environment
//! would write into the real index. [`ScratchRepo::repo`] hands a check the scratch repository as
//! a [`Repo::scratch`], whose listings are isolated the same way.
//!
//! Like [`crate::Repo`], the scratch helpers panic when git or the file system fails: a harness
//! that cannot build its fixture has nothing to say about the script. The verdicts are problems.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/suppressions.rs at c9e79afa8810 with
//! swarm-device-access/meta/tests/suppressions.rs at 8b4ab3a3e417 (`Scratch`, its baseline and
//! commits under any file name, `ratchet`, the cases and `run_cases`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use testenv_core::gitenv;

use crate::{Problem, Repo};

/// A scratch git repository on branch `main`, with an identity and no commit signing, so
/// committing needs no global configuration; removed when dropped.
#[derive(Debug)]
pub struct ScratchRepo {
    dir: tempfile::TempDir,
}

impl ScratchRepo {
    /// An empty repository.
    ///
    /// # Panics
    ///
    /// The directory cannot be made, or git cannot initialise it.
    #[must_use]
    pub fn new() -> Self {
        let scratch = Self {
            dir: tempfile::tempdir().unwrap_or_else(|error| panic!("scratch directory: {error}")),
        };
        scratch.git(&["init", "-q", "-b", "main"]);
        scratch.git(&["config", "user.email", "scratch@example.invalid"]);
        scratch.git(&["config", "user.name", "Scratch Repository"]);
        scratch.git(&["config", "commit.gpgsign", "false"]);
        scratch
    }

    /// The repository's directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The repository as a [`Repo::scratch`]: its listings read its own index whatever git
    /// environment the process carries, so a check run on it as a negative control never sees the
    /// real repository's.
    #[must_use]
    pub fn repo(&self) -> Repo {
        Repo::scratch(self.path())
    }

    /// Writes one file, creating the directories above it.
    ///
    /// # Panics
    ///
    /// The file cannot be written.
    pub fn write(&self, name: &str, content: &str) {
        let path = self.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("create the directory of {name}: {error}"));
        }
        fs::write(&path, content).unwrap_or_else(|error| panic!("write {name}: {error}"));
    }

    /// Reads one file.
    ///
    /// # Panics
    ///
    /// The file cannot be read.
    #[must_use]
    pub fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path().join(name))
            .unwrap_or_else(|error| panic!("read {name}: {error}"))
    }

    /// Runs git here, isolated.
    ///
    /// # Panics
    ///
    /// git cannot run, or fails.
    pub fn git(&self, args: &[&str]) {
        let output = gitenv::isolate(Command::new("git").args(args).current_dir(self.path()))
            .output()
            .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
        assert!(
            output.status.success(),
            "git {}: {}",
            args.join(" "),
            combined(&output)
        );
    }

    /// Runs `program` with `args` here, isolated: whether it succeeded, and its standard output
    /// followed by its standard error.
    ///
    /// # Panics
    ///
    /// The program cannot be started.
    #[must_use]
    pub fn run(&self, program: &Path, args: &[&str]) -> (bool, String) {
        let output = gitenv::isolate(Command::new(program).args(args).current_dir(self.path()))
            .output()
            .unwrap_or_else(|error| panic!("run {}: {error}", program.display()));
        (output.status.success(), combined(&output))
    }
}

impl Default for ScratchRepo {
    fn default() -> Self {
        Self::new()
    }
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The suppression ratchet of a repository, run in scratch repositories from where it lives.
#[derive(Clone, Copy, Debug)]
pub struct SuppressionHarness<'harness> {
    /// The ratchet, relative to the root; it runs from the repository under test with the scratch
    /// repository as its working directory.
    pub script: &'harness str,
    /// The baseline file the ratchet reads, relative to the scratch root, such as
    /// `.suppressions-baseline`.
    pub baseline: &'harness str,
}

/// One scratch repository for the ratchet, with its baseline helpers.
#[derive(Debug)]
pub struct RatchetScratch {
    repo: ScratchRepo,
    script: PathBuf,
    baseline: String,
}

/// A scratch state the ratchet must accept (`None`) or refuse with output containing the given
/// text, so a case cannot pass on the wrong refusal.
pub type RatchetCase = (&'static str, fn(&RatchetScratch), Option<&'static str>);

impl SuppressionHarness<'_> {
    /// An empty scratch repository whose ratchet is `repo`'s.
    #[must_use]
    pub fn scratch(&self, repo: &Repo) -> RatchetScratch {
        RatchetScratch {
            repo: ScratchRepo::new(),
            script: repo.path(self.script),
            baseline: self.baseline.to_owned(),
        }
    }

    /// Runs each case: a fresh scratch repository, the case's set-up, then the ratchet with no
    /// argument. A problem for every case whose verdict or refusal text is wrong, for a refusal
    /// text that is empty (it would match any refusal), when the ratchet is not a file, and when
    /// there is no case.
    ///
    /// # Panics
    ///
    /// A set-up step or the ratchet cannot run.
    #[must_use]
    pub fn run_cases(&self, repo: &Repo, cases: &[RatchetCase]) -> Vec<Problem> {
        if !repo.path(self.script).is_file() {
            return vec![Problem::at(self.script, "is not a file in the repository")];
        }
        if cases.is_empty() {
            return vec![Problem::at(
                self.script,
                "no case; the harness would pass on nothing",
            )];
        }
        let mut problems = Vec::new();
        for (name, _, refusal) in cases {
            if refusal.is_some_and(|text| text.trim().is_empty()) {
                problems.push(Problem::at(
                    *name,
                    "expects a refusal with empty or blank text, which any refusal matches",
                ));
            }
        }
        if !problems.is_empty() {
            return problems;
        }
        for (name, set_up, refusal) in cases {
            let scratch = self.scratch(repo);
            set_up(&scratch);
            let (accepted, output) = scratch.ratchet(&[]);
            match refusal {
                None if !accepted => problems.push(Problem::at(
                    *name,
                    format!("refused a tree it should accept:\n{output}"),
                )),
                Some(_) if accepted => problems.push(Problem::at(
                    *name,
                    format!("accepted the tree, want a refusal:\n{output}"),
                )),
                Some(message) if !output.contains(message) => problems.push(Problem::at(
                    *name,
                    format!("output does not mention {message:?}:\n{output}"),
                )),
                None | Some(_) => {}
            }
        }
        problems
    }
}

impl RatchetScratch {
    /// The scratch repository.
    #[must_use]
    pub fn repo(&self) -> &ScratchRepo {
        &self.repo
    }

    /// Writes one file.
    ///
    /// # Panics
    ///
    /// See [`ScratchRepo::write`].
    pub fn write(&self, name: &str, content: &str) {
        self.repo.write(name, content);
    }

    /// Reads one file.
    ///
    /// # Panics
    ///
    /// See [`ScratchRepo::read`].
    #[must_use]
    pub fn read(&self, name: &str) -> String {
        self.repo.read(name)
    }

    /// Runs git here.
    ///
    /// # Panics
    ///
    /// See [`ScratchRepo::git`].
    pub fn git(&self, args: &[&str]) {
        self.repo.git(args);
    }

    /// Writes a baseline holding `lines` (one `<family>/<rule>\t<count>` per line), after the
    /// comment a real one starts with.
    pub fn write_baseline(&self, lines: &str) {
        let mut content = "# scratch baseline\n".to_owned();
        if !lines.is_empty() {
            content.push_str(lines);
            content.push('\n');
        }
        self.repo.write(&self.baseline, &content);
    }

    /// Writes `source` as `name` and a baseline of `baseline`, and commits both, leaving a clean
    /// index.
    pub fn commit_initial(&self, name: &str, source: &str, baseline: &str) {
        self.write(name, source);
        self.write_baseline(baseline);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "scratch fixture"]);
    }

    /// Commits an empty baseline, leaving a clean index.
    pub fn commit_empty_baseline(&self) {
        self.write_baseline("");
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "empty baseline"]);
    }

    /// Runs the ratchet here with `args`: whether it accepted the tree, and its output.
    ///
    /// # Panics
    ///
    /// The ratchet cannot be started.
    #[must_use]
    pub fn ratchet(&self, args: &[&str]) -> (bool, String) {
        self.repo.run(&self.script, args)
    }
}

#[cfg(test)]
mod tests;
