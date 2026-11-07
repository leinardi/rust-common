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

//! The workspace checks on scratch manifests: each way to break them is found, and a workspace
//! that keeps them passes.
//!
//! Extracted from swarm-device-access/meta/tests/workspace_lints.rs at 8b4ab3a3e417 and
//! `bitgauge/meta/tests/lint_inheritance.rs` at 600fc3905200 (the negative controls).

use std::fs;
use std::path::Path;

use toml::Table;

use super::{lint_opt_in, member_opt_in_problems, root_lint_problems};
use crate::{Problem, Repo, strings};

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn table(text: &str) -> Table {
    text.parse().unwrap()
}

#[test]
fn lint_check_is_not_blind() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"meta\", \"crates/alpha\", \"crates/*\", \"crates/gone\"]\n\n\
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
    let problems = strings(&lint_opt_in(&Repo::scratch(root), None));
    assert_eq!(problems.len(), 4, "{problems:#?}");
    assert_eq!(problems[0], "[workspace.lints.clippy] is missing");
    assert_eq!(
        problems[1],
        "crates/alpha/Cargo.toml does not declare [lints] workspace = true"
    );
    assert_eq!(
        problems[2],
        r#"member "crates/*" is a glob; list members explicitly so this check sees each one"#
    );
    assert!(
        problems[3].starts_with("crates/gone/Cargo.toml: cannot be read"),
        "{problems:?}"
    );
}

#[test]
fn a_workspace_whose_members_opt_in_passes() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\".\", \"crates/alpha\"]\n\n[workspace.lints.rust]\n\
         [workspace.lints.clippy]\n\n[package]\nname = \"root\"\n\n[lints]\nworkspace = true\n",
    );
    write(
        root,
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\n\n[lints]\nworkspace = true\n",
    );
    let repo = Repo::scratch(root);
    assert_eq!(lint_opt_in(&repo, None), []);
    assert_eq!(lint_opt_in(&repo, Some(&[".", "crates/alpha"])), []);
    assert_eq!(
        strings(&lint_opt_in(&repo, Some(&[".", "meta", "crates/alpha"]))),
        [
            r#"the workspace members are [".", "crates/alpha"]; want exactly [".", "meta", "crates/alpha"]"#
        ]
    );
}

/// A root without the lint tables, and a member that does not opt in or opts out.
#[test]
fn roots_and_members_without_the_opt_in_are_refused() {
    assert_eq!(
        strings(&root_lint_problems(&table(
            "[workspace]\nmembers = [\".\"]\n"
        ))),
        ["the root Cargo.toml has no [workspace.lints] table"]
    );
    assert_eq!(
        strings(&root_lint_problems(&table(
            "[workspace.lints.clippy]\npedantic = \"warn\"\n"
        ))),
        ["[workspace.lints.rust] is missing"]
    );
    let root = table("[workspace]\nmembers = [\".\", \"silent\", \"opted-out\"]\n");
    let problems = member_opt_in_problems(
        &root,
        |member| {
            Ok(match member {
                "." => table("[lints]\nworkspace = true\n"),
                "opted-out" => table("[lints]\nworkspace = false\n"),
                _ => table("[package]\nname = \"silent\"\n"),
            })
        },
        None,
    );
    assert_eq!(
        strings(&problems),
        [
            "silent/Cargo.toml does not declare [lints] workspace = true",
            "opted-out/Cargo.toml does not declare [lints] workspace = true",
        ]
    );
    let read_nothing = |_: &str| -> Result<Table, Problem> { Ok(Table::new()) };
    for (manifest, want) in [
        (
            "[package]\nname = \"x\"\n",
            "the root Cargo.toml lists no [workspace] members",
        ),
        (
            "[workspace]\nmembers = []\n",
            "the workspace lists no members",
        ),
        (
            "[workspace]\nmembers = [1]\n",
            "workspace member 1 is not a path string",
        ),
    ] {
        assert_eq!(
            strings(&member_opt_in_problems(
                &table(manifest),
                read_nothing,
                None
            )),
            [want]
        );
    }
}
