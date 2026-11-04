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
//! Extracted from swarm-scheduler-exporter/meta/src/repo.rs at c9e79afa8810, without
//! `files_under`, which nothing here needs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The repository root: the meta crate sits one level below it.
#[must_use]
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The absolute path of a slash-separated path relative to the root.
#[must_use]
pub fn path(relative: &str) -> PathBuf {
    root().join(relative)
}

/// Reads a file by its slash-separated path relative to the root.
///
/// # Panics
///
/// The file cannot be read, naming it.
#[must_use]
pub fn read(relative: &str) -> String {
    fs::read_to_string(path(relative)).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// The tracked files matching `pathspecs`, as slash-separated paths relative to the root. Run
/// with the repository's own git environment: under the commit hook it names the index being
/// committed.
///
/// # Panics
///
/// git cannot list them.
#[must_use]
pub fn tracked(pathspecs: &[&str]) -> Vec<String> {
    let output = Command::new("git")
        .arg("ls-files")
        .arg("-z")
        .arg("--")
        .args(pathspecs)
        .current_dir(root())
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
