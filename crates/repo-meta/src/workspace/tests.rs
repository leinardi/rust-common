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
//! Extracted from swarm-device-access/meta/tests/workspace_lints.rs at 8b4ab3a3e417,
//! `bitgauge/meta/tests/lint_inheritance.rs` at 600fc3905200 and
//! swarm-device-access/meta/tests/unsafe_allowlist.rs at 8b4ab3a3e417 (the negative controls).

use std::fs;
use std::path::Path;

use toml::Table;

use super::{
    LintLevel, crate_of_manifest, layout_problems, lint_level_problems, lint_opt_in,
    member_opt_in_problems, override_problems, root_lint_problems,
};
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

/// Only the exact level passes, in either form; a missing one is refused.
#[test]
fn lint_levels() {
    let deny = [LintLevel {
        table: "rust",
        lint: "unsafe_code",
        level: "deny",
    }];
    for (level, refused) in [
        ("\"deny\"", false),
        ("{ level = \"deny\", priority = 0 }", false),
        ("\"forbid\"", true),
        ("\"warn\"", true),
        ("\"allow\"", true),
    ] {
        let root = table(&format!("[workspace.lints.rust]\nunsafe_code = {level}\n"));
        assert_eq!(
            !lint_level_problems(&root, &deny).is_empty(),
            refused,
            "level {level}"
        );
    }
    assert_eq!(
        strings(&lint_level_problems(
            &table("[workspace.lints.rust]\n"),
            &deny
        )),
        ["[workspace.lints.rust] does not set unsafe_code = \"deny\""]
    );
    let clippy = [LintLevel {
        table: "clippy",
        lint: "disallowed_methods",
        level: "deny",
    }];
    assert_eq!(
        strings(&lint_level_problems(
            &table("[workspace.lints.clippy]\ndisallowed_methods = \"warn\"\n"),
            &clippy
        )),
        ["[workspace.lints.clippy] sets disallowed_methods = \"warn\"; it must be \"deny\""]
    );
}

/// Cargo configuration or make that lowers a lint, its group or every lint, or points clippy
/// elsewhere, is refused; a plain one passes.
#[test]
fn overrides_are_refused() {
    let tokens = [
        "CLIPPY_CONF_DIR",
        "--cap-lints",
        "clippy::style",
        "disallowed_methods",
        "unsafe_code",
    ];
    for text in [
        "[build]\nrustflags = [\"-A\", \"clippy::disallowed_methods\"]\n",
        "[build]\nrustflags = [\"-A\", \"clippy::style\"]\n",
        "[build]\nrustflags = [\"--cap-lints\", \"allow\"]\n",
        "[env]\nCLIPPY_CONF_DIR = \"/elsewhere\"\n",
        "RUSTFLAGS += -A unsafe_code\n",
    ] {
        let files = [(".cargo/config.toml".to_owned(), text.to_owned())];
        assert_eq!(override_problems(&files, &tokens).len(), 1, "{text}");
    }
    let files = [(
        ".cargo/config.toml".to_owned(),
        "[build]\njobs = 4\n".to_owned(),
    )];
    assert_eq!(override_problems(&files, &tokens), []);
}

#[test]
fn layout_check_is_not_blind() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"meta\", \"tools/xtask\", \"crates/alpha\", \"crates/gone\", \
         \"crates/alpha/nested\"]\n",
    );
    assert_eq!(
        strings(&layout_problems(
            &Repo::scratch(root),
            "crates",
            &["meta"],
            &["alpha".to_owned(), "beta".to_owned()]
        )),
        [
            r#"workspace member "tools/xtask" is neither meta nor crates/<name>"#,
            r#"workspace member "crates/alpha/nested" is neither meta nor crates/<name>"#,
            "crates/beta/Cargo.toml is tracked but crates/beta is not a workspace member",
            "workspace member crates/gone has no tracked crates/gone/Cargo.toml",
        ]
    );
}

#[test]
fn a_matching_layout_passes() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\", \"meta\"]\n",
    );
    assert_eq!(
        layout_problems(
            &Repo::scratch(root),
            "crates",
            &["meta"],
            &["alpha".to_owned()]
        ),
        []
    );
    assert_eq!(
        crate_of_manifest("crates", "crates/a/Cargo.toml"),
        Some("a")
    );
    assert_eq!(crate_of_manifest("crates", "crates/a/b/Cargo.toml"), None);
    assert_eq!(crate_of_manifest("crates", "cratesx/a/Cargo.toml"), None);
    assert_eq!(crate_of_manifest("crates", "crates//Cargo.toml"), None);
}

/// No file or no token is a problem, not a pass on nothing.
#[test]
fn an_empty_override_scan_is_a_problem() {
    let files = [("Makefile".to_owned(), "all:\n".to_owned())];
    for (files, tokens) in [(&files[..], &[][..]), (&[][..], &["--cap-lints"][..])] {
        assert_eq!(
            strings(&override_problems(files, tokens)),
            ["no file or no token to look for; the override check would pass on nothing"]
        );
    }
}

/// No required lint level and no tracked crate are problems, not passes on nothing.
#[test]
fn empty_level_and_layout_inputs_are_problems() {
    let root: Table = "[workspace]\nmembers = [\"meta\"]\n".parse().unwrap();
    assert_eq!(
        strings(&lint_level_problems(&root, &[])),
        ["no lint level is required; the check would pass on nothing"]
    );
    let scratch = tempfile::tempdir().unwrap();
    fs::write(
        scratch.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"meta\"]\n",
    )
    .unwrap();
    assert_eq!(
        strings(&layout_problems(
            &Repo::scratch(scratch.path()),
            "crates",
            &["meta"],
            &[]
        )),
        ["no crate under crates has a tracked manifest; the check would pass on nothing"]
    );
}
