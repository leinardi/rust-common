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

//! The shared lint set is declared once, in the root `[workspace.lints]`, and a package only obeys
//! it when its own manifest opts in: `[lints]` is not inherited by workspace members. A member that
//! forgets `[lints] workspace = true` builds and passes clippy with the default lints, silently.
//! The check is `meta::workspace::lint_problems`; the negative control runs it on a scratch tree.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/workspace_lints.rs at c9e79afa8810.

use std::fs;
use std::path::Path;

use meta::repo;
use meta::workspace::lint_problems;

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| panic!("create {relative}: {error}"));
    }
    fs::write(&path, content).unwrap_or_else(|error| panic!("write {relative}: {error}"));
}

#[test]
fn every_workspace_member_opts_into_the_workspace_lints() {
    let problems = lint_problems(&repo::root());
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn lint_check_is_not_blind() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"meta\", \"crates/alpha\", \"crates/*\"]\n\n\
         [workspace.lints.rust]\nunsafe_code = \"forbid\"\n",
    );
    write(
        root,
        "meta/Cargo.toml",
        "[package]\nname = \"meta\"\n\n[lints]\nworkspace = true\n",
    );
    write(
        root,
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\n\n[lints.clippy]\npedantic = \"warn\"\n",
    );

    assert_eq!(
        lint_problems(root),
        [
            "[workspace.lints.clippy] is missing",
            "crates/alpha/Cargo.toml does not declare [lints] workspace = true",
            r#"member "crates/*" is a glob; list members explicitly so this check sees each one"#,
        ]
    );
}

#[test]
fn a_workspace_whose_members_opt_in_passes() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\"]\n\n[workspace.lints.rust]\n\
         [workspace.lints.clippy]\n",
    );
    write(
        root,
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\n\n[lints]\nworkspace = true\n",
    );

    let problems = lint_problems(root);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
