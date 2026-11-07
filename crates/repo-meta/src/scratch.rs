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

//! Scratch git repositories, for the checks and meta-tests that need a tree of their own: a
//! negative control builds the repository it plants a violation in.
//!
//! Every git command and every script run in a scratch repository goes through
//! [`testenv_core::gitenv::isolate`]: under a commit hook, git exports the location of the
//! repository being committed, and a `git add` in a scratch repository with that environment
//! would write into the real index. [`ScratchRepo::repo`] hands a check the scratch repository as
//! a [`Repo::scratch`], whose listings are isolated the same way.
//!
//! Like [`crate::Repo`], the scratch helpers panic when git or the file system fails: a harness
//! that cannot build its fixture has nothing to say about the repository.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use testenv_core::gitenv;

use crate::Repo;

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
