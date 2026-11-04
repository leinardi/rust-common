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

//! The repository under test: its root, its files and its tracked paths.
//!
//! A meta crate sits one level below the root, so a test names the root with
//! `Repo::new(testenv_core::repo_root!())`: the macro expands `CARGO_MANIFEST_DIR` in the caller.
//! Any other root works the same, which is how the negative controls point a check at a scratch
//! tree.
//!
//! These helpers are for the tests themselves, so they panic, naming the path, when a file cannot
//! be read or git cannot list: a meta-test that cannot see the repository fails. The checks of
//! this crate never panic; they take contents, or read through [`Repo::try_read`] and report what
//! they cannot read as a [`Problem`].
//!
//! Extracted from swarm-scheduler-exporter/meta/src/repo.rs at c9e79afa8810 (`root`, `path`,
//! `read`, `tracked`, `files_under`), swarm-device-access/meta/src/repo.rs at 8b4ab3a3e417
//! (`tracked_or_new`) and bitgauge/meta/src/repo.rs at 600fc3905200 (`tracked_or_new` keeps only
//! the files that exist, so a tracked file deleted in the worktree is not read).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Problem;

/// A repository root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// The repository at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The absolute path of a slash-separated path relative to the root.
    #[must_use]
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Reads a file by its slash-separated path relative to the root.
    ///
    /// # Panics
    ///
    /// The file cannot be read, naming it.
    #[must_use]
    pub fn read(&self, relative: &str) -> String {
        self.try_read(relative)
            .unwrap_or_else(|problem| panic!("{problem}"))
    }

    /// Reads a file by its slash-separated path relative to the root.
    ///
    /// # Errors
    ///
    /// The file cannot be read: a problem at the path.
    pub fn try_read(&self, relative: &str) -> Result<String, Problem> {
        fs::read_to_string(self.path(relative))
            .map_err(|error| Problem::at(relative, format!("cannot be read: {error}")))
    }

    /// Each of `paths` with its content, in order.
    ///
    /// # Panics
    ///
    /// A file cannot be read, naming it.
    #[must_use]
    pub fn read_all(&self, paths: &[String]) -> Vec<(String, String)> {
        paths
            .iter()
            .map(|path| (path.clone(), self.read(path)))
            .collect()
    }

    /// The tracked files matching `pathspecs`, as slash-separated paths relative to the root, in
    /// git's order. Run with the repository's own git environment: under the commit hook it names
    /// the index being committed.
    ///
    /// # Panics
    ///
    /// git cannot list them.
    #[must_use]
    pub fn tracked(&self, pathspecs: &[&str]) -> Vec<String> {
        self.ls_files(&[], pathspecs)
    }

    /// The files matching `pathspecs` that are tracked or new (untracked and not ignored) and
    /// exist in the worktree, sorted: what the next `git add -A` would commit, so a check that
    /// must hold for every source file also sees one nobody has added yet, and does not trip on a
    /// tracked file that is being deleted.
    ///
    /// # Panics
    ///
    /// git cannot list them.
    #[must_use]
    pub fn tracked_or_new(&self, pathspecs: &[&str]) -> Vec<String> {
        self.ls_files(&["--cached", "--others", "--exclude-standard"], pathspecs)
            .into_iter()
            .filter(|path| self.path(path).is_file())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn ls_files(&self, options: &[&str], pathspecs: &[&str]) -> Vec<String> {
        let output = Command::new("git")
            .arg("ls-files")
            .arg("-z")
            .args(options)
            .arg("--")
            .args(pathspecs)
            .current_dir(&self.root)
            .output()
            .unwrap_or_else(|error| panic!("run git ls-files: {error}"));
        assert!(
            output.status.success(),
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Every file under `directory` (relative to the root, recursively, symlinks not followed)
    /// whose name ends with one of `suffixes`, as slash-separated paths relative to the root,
    /// sorted.
    ///
    /// # Panics
    ///
    /// The directory cannot be read: a missing one too, so a scan that would see nothing fails.
    #[must_use]
    pub fn files_under(&self, directory: &str, suffixes: &[&str]) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![directory.trim_end_matches('/').to_owned()];
        while let Some(current) = pending.pop() {
            let entries = fs::read_dir(self.path(&current))
                .unwrap_or_else(|error| panic!("list {current}: {error}"));
            for entry in entries {
                let entry = entry.unwrap_or_else(|error| panic!("list {current}: {error}"));
                let name = entry.file_name().to_string_lossy().into_owned();
                let relative = format!("{current}/{name}");
                let kind = entry
                    .file_type()
                    .unwrap_or_else(|error| panic!("inspect {relative}: {error}"));
                if kind.is_dir() {
                    pending.push(relative);
                } else if kind.is_file() && suffixes.iter().any(|suffix| name.ends_with(suffix)) {
                    found.push(relative);
                }
            }
        }
        found.sort();
        found
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::Repo;

    fn git(repo: &Repo, args: &[&str]) {
        let status =
            testenv_core::gitenv::isolate(Command::new("git").args(args).current_dir(repo.root()))
                .status()
                .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// The listings over a scratch repository: tracked only, tracked or new (sorted, without the
    /// ignored and the deleted), and a directory walk by suffix.
    #[test]
    fn listings_see_what_they_promise() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repo::new(scratch.path());
        for (path, content) in [
            ("b.rs", "b"),
            ("a/x.rs", "x"),
            ("gone.rs", "g"),
            (".gitignore", "ignored.rs\n"),
        ] {
            let file = repo.path(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["add", "b.rs", "gone.rs", ".gitignore"]);
        fs::write(repo.path("new.rs"), "n").unwrap();
        fs::write(repo.path("ignored.rs"), "i").unwrap();
        fs::remove_file(repo.path("gone.rs")).unwrap();

        assert_eq!(repo.tracked(&["*.rs"]), ["b.rs", "gone.rs"]);
        assert_eq!(repo.tracked_or_new(&["*.rs"]), ["a/x.rs", "b.rs", "new.rs"]);
        assert_eq!(
            repo.files_under("a", &[".rs"]),
            ["a/x.rs"],
            "files_under walks the tree"
        );
        assert_eq!(repo.read("b.rs"), "b");
        assert_eq!(
            repo.try_read("gone.rs").unwrap_err().location,
            "gone.rs",
            "a missing file is a problem at its path"
        );
        assert_eq!(
            repo.read_all(&["b.rs".to_owned()]),
            [("b.rs".to_owned(), "b".to_owned())]
        );
    }

    #[test]
    #[should_panic(expected = "list missing")]
    fn a_missing_directory_fails_the_walk() {
        let scratch = tempfile::tempdir().unwrap();
        let _ = Repo::new(scratch.path()).files_under("missing", &[".rs"]);
    }
}
