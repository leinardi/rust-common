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
//! `Repo::new(testenv_core::repo_root!())`: the macro expands `CARGO_MANIFEST_DIR` in the caller,
//! at compile time. The root is the checkout the test binary was built from, so a binary built
//! in one checkout (a scratch copy, a second worktree) and run from a shared target directory
//! checks that checkout, or a path that no longer exists; each checkout builds into its own
//! target directory.
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

/// A repository root, and whether its git commands keep the process's git environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    root: PathBuf,
    isolated: bool,
}

/// The value a plain `git commit` in a main checkout exports as `GIT_INDEX_FILE`.
const COMMIT_INDEX: &str = ".git/index";

/// Removes a `GIT_INDEX_FILE` of exactly `.git/index` from `command`, run at `root`, when `root`'s
/// top level has no `.git` directory.
///
/// A plain `git commit` in a main checkout exports the index being committed as the relative
/// `.git/index` (`commit -a`, `commit <path>` and a linked worktree export absolute paths), and git
/// resolves a relative value against the top level of the checkout it finds. In the committing
/// checkout that is the index being committed. A root that is another checkout, one whose `.git` is
/// a file (a linked worktree or a submodule), cannot hold such an index, and git fails with
/// `.git/index: ... Not a directory`; its own index is git's default, so the variable is removed.
/// Any other value, absolute or a relative custom one such as `alternate.index`, is left alone.
pub(crate) fn drop_unusable_relative_index(command: &mut Command, root: &Path) {
    if std::env::var_os("GIT_INDEX_FILE").is_none_or(|value| value != COMMIT_INDEX) {
        return;
    }
    let mut probe = Command::new("git");
    probe
        .current_dir(root)
        .env_remove("GIT_INDEX_FILE")
        .args(["rev-parse", "--show-toplevel"]);
    let top_level = probe
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(|| root.to_path_buf(), |line| PathBuf::from(line.trim_end()));
    if !top_level.join(".git").is_dir() {
        command.env_remove("GIT_INDEX_FILE");
    }
}

impl Repo {
    /// The repository under test at `root`. Its git commands keep the process's git environment:
    /// under a commit hook, git exports the index being committed (`GIT_INDEX_FILE`, and `GIT_DIR`
    /// in a worktree), and the listings read that index. The index is absolute except in a plain
    /// `git commit` in a main checkout, which exports `.git/index`; git resolves that against the
    /// top level of the checkout it finds, so it is dropped for a root whose `.git` is a file (see
    /// [`drop_unusable_relative_index`]).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            isolated: false,
        }
    }

    /// A scratch repository at `root`, for a negative control. Its git commands go through
    /// [`testenv_core::gitenv::isolate`], so a commit hook's environment, which names the real
    /// repository, never reaches them and the listings read the scratch repository's own index.
    #[must_use]
    pub fn scratch(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            isolated: true,
        }
    }

    /// Whether this is a scratch repository whose git commands are isolated ([`Repo::scratch`]).
    #[must_use]
    pub fn is_isolated(&self) -> bool {
        self.isolated
    }

    /// A git command run at the root, isolated for a scratch repository.
    #[must_use]
    pub fn git_command(&self) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.root);
        if self.isolated {
            testenv_core::gitenv::isolate(&mut command);
        } else {
            drop_unusable_relative_index(&mut command, &self.root);
        }
        command
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
    /// git's order. A repository under test ([`Repo::new`]) lists with the process's own git
    /// environment, so under the commit hook it names the index being committed; a scratch one
    /// ([`Repo::scratch`]) lists its own index whatever the environment says.
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

    /// The names git lists. A name that is not UTF-8 fails the listing, naming its bytes: a
    /// lossy conversion would hand a check a path that does not exist, and the file would escape
    /// it.
    fn ls_files(&self, options: &[&str], pathspecs: &[&str]) -> Vec<String> {
        let output = self
            .git_command()
            .arg("ls-files")
            .arg("-z")
            .args(options)
            .arg("--")
            .args(pathspecs)
            .output()
            .unwrap_or_else(|error| panic!("run git ls-files: {error}"));
        assert!(
            output.status.success(),
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| {
                String::from_utf8(name.to_vec()).unwrap_or_else(|_| {
                    panic!(
                        "git lists a path that is not UTF-8, which no check can read: {}",
                        name.escape_ascii()
                    )
                })
            })
            .collect()
    }

    /// Every file under `directory` (relative to the root, recursively) whose name ends with one
    /// of `suffixes`, as slash-separated paths relative to the root, sorted. A symlink counts as
    /// what it points to when that is a file, and as a file when it points nowhere (reading it
    /// then fails, as it would for a check); a symlinked directory is not entered, so a link back
    /// up the tree cannot loop the walk.
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
                    continue;
                }
                let file = kind.is_file()
                    || (kind.is_symlink()
                        && fs::metadata(self.path(&relative))
                            .ok()
                            .is_none_or(|target| target.is_file()));
                if file && suffixes.iter().any(|suffix| name.ends_with(suffix)) {
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
    use std::path::Path;
    use std::process::Command;

    use super::Repo;

    fn git(repo: &Repo, args: &[&str]) {
        let status = repo.git_command().args(args).status().unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn write(repo: &Repo, path: &str, content: &str) {
        let file = repo.path(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    /// The listings over a scratch repository: tracked only, tracked or new (sorted, without the
    /// ignored and the deleted), and a directory walk by suffix.
    #[test]
    fn listings_see_what_they_promise() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repo::scratch(scratch.path());
        assert!(repo.is_isolated() && !Repo::new(scratch.path()).is_isolated());
        for (path, content) in [
            ("b.rs", "b"),
            ("a/x.rs", "x"),
            ("gone.rs", "g"),
            (".gitignore", "ignored.rs\n"),
        ] {
            write(&repo, path, content);
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

    /// A symlink to a file is walked as the file, and a dangling one as a file; a symlinked
    /// directory is not entered, so a link back up the tree ends the walk.
    #[test]
    fn the_walk_follows_a_symlink_to_a_file() {
        use std::os::unix::fs::symlink;

        let scratch = tempfile::tempdir().unwrap();
        let repo = Repo::scratch(scratch.path());
        write(&repo, "shared/ci.yaml", "on: push\n");
        write(&repo, "w/real.yaml", "on: push\n");
        symlink("../shared/ci.yaml", repo.path("w/linked.yaml")).unwrap();
        symlink("../shared/missing.yaml", repo.path("w/dangling.yaml")).unwrap();
        symlink("..", repo.path("w/up.yaml")).unwrap();
        symlink("../shared", repo.path("w/sub")).unwrap();
        assert_eq!(
            repo.files_under("w", &[".yaml"]),
            ["w/dangling.yaml", "w/linked.yaml", "w/real.yaml"]
        );
        assert_eq!(repo.read("w/linked.yaml"), "on: push\n");
    }

    #[test]
    #[should_panic(expected = "list missing")]
    fn a_missing_directory_fails_the_walk() {
        let scratch = tempfile::tempdir().unwrap();
        let _ = Repo::scratch(scratch.path()).files_under("missing", &[".rs"]);
    }

    /// A tracked name that is not UTF-8 fails the listing rather than reach a check mangled.
    #[test]
    #[should_panic(expected = "not UTF-8")]
    fn a_name_that_is_not_utf8_fails_the_listing() {
        use std::os::unix::ffi::OsStrExt as _;
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repo::scratch(scratch.path());
        git(&repo, &["init", "--quiet"]);
        let name = std::ffi::OsStr::from_bytes(b"bad-\xff.rs");
        fs::write(scratch.path().join(name), "x").unwrap();
        let status = repo
            .git_command()
            .arg("add")
            .arg("--")
            .arg(name)
            .status()
            .unwrap();
        assert!(status.success());
        let _ = repo.tracked_or_new(&["*.rs"]);
    }

    /// The child half of [`a_commit_hook_environment_does_not_reach_a_scratch_repository`]: a
    /// scratch repository lists its own index. Under the parent's environment, a repository
    /// under test at the same root lists the decoy's instead, which shows the environment
    /// arrived.
    #[test]
    #[ignore = "run by a_commit_hook_environment_does_not_reach_a_scratch_repository"]
    fn scratch_listing_under_a_hook_environment() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repo::scratch(scratch.path());
        git(&repo, &["init", "--quiet"]);
        write(&repo, "own.rs", "o");
        git(&repo, &["add", "own.rs"]);
        assert_eq!(repo.tracked(&["*.rs"]), ["own.rs"]);
        assert_eq!(repo.tracked_or_new(&["*.rs"]), ["own.rs"]);
        if std::env::var_os("GIT_INDEX_FILE").is_some() {
            assert_eq!(Repo::new(scratch.path()).tracked(&["*.rs"]), ["decoy.rs"]);
        }
    }

    /// The child half of the relative-index tests: the repository under test at `LISTING_ROOT`
    /// lists `LISTING_EXPECTED` (comma-separated) under the parent's `GIT_INDEX_FILE`.
    #[test]
    #[ignore = "run by the relative-index tests"]
    fn listing_under_a_relative_index() {
        let root = std::env::var_os("LISTING_ROOT").unwrap();
        let expected = std::env::var("LISTING_EXPECTED").unwrap();
        let expected: Vec<&str> = expected
            .split(',')
            .filter(|name| !name.is_empty())
            .collect();
        assert_eq!(Repo::new(root).tracked(&["*.rs"]), expected);
    }

    fn decoy() -> tempfile::TempDir {
        let decoy = tempfile::tempdir().unwrap();
        let repo = Repo::scratch(decoy.path());
        git(&repo, &["init", "--quiet"]);
        write(&repo, "decoy.rs", "d");
        git(&repo, &["add", "decoy.rs"]);
        decoy
    }

    fn run_child(env: &[(&str, &Path)]) {
        run_named_child("scratch_listing_under_a_hook_environment", env);
    }

    fn run_named_child(child_test: &str, env: &[(&str, &Path)]) {
        let mut child = Command::new(std::env::current_exe().unwrap());
        // The parent's own git environment (a commit hook's) is dropped, so the child sees only
        // the decoy's.
        testenv_core::gitenv::isolate(&mut child);
        child.args([
            "--exact",
            &format!("repo::tests::{child_test}"),
            "--include-ignored",
            "--quiet",
        ]);
        for (name, value) in env {
            child.env(name, value);
        }
        let output = child.output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && text.contains("1 passed"),
            "{text}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Under a commit hook, git exports the real repository's index (`GIT_INDEX_FILE`, absolute
    /// except for `.git/index` in a plain commit) and, in a worktree, its git directory
    /// (`GIT_DIR`). A scratch repository's listing must not read either.
    #[test]
    fn a_commit_hook_environment_does_not_reach_a_scratch_repository() {
        let decoy = decoy();
        let git_dir = decoy.path().join(".git");
        let index = git_dir.join("index");
        run_child(&[("GIT_INDEX_FILE", &index)]);
        run_child(&[("GIT_DIR", &git_dir), ("GIT_INDEX_FILE", &index)]);
    }

    /// A main checkout at `main` with `main.rs` committed and `staged.rs` added, and a linked
    /// worktree of it at `worktree` with `own.rs` added to its own index.
    fn main_and_worktree(main: &Path, worktree: &Path) {
        let repo = Repo::scratch(main);
        git(&repo, &["init", "--quiet", "-b", "main"]);
        write(&repo, "main.rs", "m");
        git(&repo, &["add", "main.rs"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        git(
            &repo,
            &["worktree", "add", "--quiet", worktree.to_str().unwrap()],
        );
        write(&repo, "staged.rs", "s");
        git(&repo, &["add", "staged.rs"]);
        let linked = Repo::scratch(worktree);
        write(&linked, "own.rs", "o");
        git(&linked, &["add", "own.rs"]);
    }

    /// A plain `git commit` in a main checkout exports the relative `.git/index`: a repository
    /// under test at that checkout lists its default index, and one at a linked worktree, where
    /// git cannot open `.git/index`, lists the worktree's own.
    #[test]
    fn a_relative_commit_index_lists_the_checkouts_own_index() {
        let scratch = tempfile::tempdir().unwrap();
        let worktree = scratch.path().join("linked");
        let main = scratch.path().join("main");
        fs::create_dir(&main).unwrap();
        main_and_worktree(&main, &worktree);
        let run = |root: &Path, expected: &str| {
            run_named_child(
                "listing_under_a_relative_index",
                &[
                    ("GIT_INDEX_FILE", Path::new(".git/index")),
                    ("LISTING_ROOT", root),
                    ("LISTING_EXPECTED", Path::new(expected)),
                ],
            );
        };
        run(&main, "main.rs,staged.rs");
        run(&worktree, "main.rs,own.rs");
    }

    /// A relative custom index (`alternate.index`) is honoured, not dropped.
    #[test]
    fn a_relative_custom_index_is_honoured() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repo::scratch(scratch.path());
        git(&repo, &["init", "--quiet"]);
        write(&repo, "default.rs", "d");
        write(&repo, "alt.rs", "a");
        git(&repo, &["add", "default.rs"]);
        let status = repo
            .git_command()
            .env("GIT_INDEX_FILE", "alternate.index")
            .args(["add", "alt.rs"])
            .status()
            .unwrap();
        assert!(status.success());
        run_named_child(
            "listing_under_a_relative_index",
            &[
                ("GIT_INDEX_FILE", Path::new("alternate.index")),
                ("LISTING_ROOT", scratch.path()),
                ("LISTING_EXPECTED", Path::new("alt.rs")),
            ],
        );
    }
}
