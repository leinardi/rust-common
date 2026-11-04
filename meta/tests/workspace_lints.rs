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
//! The lints the repository depends on are set at their level, no make fragment or Cargo
//! configuration lowers them from outside the manifest, and `clippy.toml` is the only clippy
//! configuration. The checks are `repo_meta::workspace`'s; the negative controls run them on
//! scratch trees.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/workspace_lints.rs at c9e79afa8810, with the
//! lint levels and the override scan of `bitgauge/meta/tests/lint_inheritance.rs` at 600fc3905200.

use repo_meta::scratch::ScratchRepo;
use repo_meta::workspace::{
    LintLevel, lint_level_problems, lint_opt_in, override_problems, root_lint_problems,
};
use repo_meta::{Repo, report, strings};

/// The lints whose level this repository's rules depend on (AGENTS.md): no unsafe code, a silence
/// only through `#[expect]` with a reason, no panic or print outside tests, every enum variant
/// named, descriptive names.
const LEVELS: &[LintLevel<'static>] = &[
    LintLevel {
        table: "rust",
        lint: "unsafe_code",
        level: "forbid",
    },
    LintLevel {
        table: "clippy",
        lint: "allow_attributes",
        level: "deny",
    },
    LintLevel {
        table: "clippy",
        lint: "allow_attributes_without_reason",
        level: "deny",
    },
    LintLevel {
        table: "clippy",
        lint: "unwrap_used",
        level: "deny",
    },
    LintLevel {
        table: "clippy",
        lint: "expect_used",
        level: "deny",
    },
    LintLevel {
        table: "clippy",
        lint: "print_stdout",
        level: "deny",
    },
    LintLevel {
        table: "clippy",
        lint: "print_stderr",
        level: "deny",
    },
    LintLevel {
        table: "clippy",
        lint: "wildcard_enum_match_arm",
        level: "deny",
    },
    LintLevel {
        table: "clippy",
        lint: "min_ident_chars",
        level: "deny",
    },
];

/// What, outside the manifest, could lower a lint or point clippy at another configuration. The
/// names are spelled in pieces so this file does not name them itself.
fn override_tokens() -> Vec<String> {
    let mut tokens = vec![
        "CLIPPY_CONF_DIR".to_owned(),
        "--cap-lints".to_owned(),
        format!("clippy::{}", "pedantic"),
        format!("clippy::{}", "all"),
    ];
    tokens.extend(LEVELS.iter().map(|level| level.lint.to_owned()));
    tokens
}

fn root_manifest(content: &str) -> toml::Table {
    content
        .parse()
        .unwrap_or_else(|error| panic!("parse the manifest: {error}"))
}

#[test]
fn every_workspace_member_opts_into_the_workspace_lints() {
    let problems = lint_opt_in(&meta::repo(), None);
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn lint_check_is_not_blind() {
    let scratch = ScratchRepo::new();
    scratch.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"meta\", \"crates/alpha\", \"crates/*\"]\n\n\
         [workspace.lints.rust]\nunsafe_code = \"forbid\"\n",
    );
    scratch.write(
        "meta/Cargo.toml",
        "[package]\nname = \"meta\"\n\n[lints]\nworkspace = true\n",
    );
    scratch.write(
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\n\n[lints.clippy]\npedantic = \"warn\"\n",
    );

    assert_eq!(
        strings(&lint_opt_in(&Repo::new(scratch.path()), None)),
        [
            "[workspace.lints.clippy] is missing",
            "crates/alpha/Cargo.toml does not declare [lints] workspace = true",
            r#"member "crates/*" is a glob; list members explicitly so this check sees each one"#,
        ]
    );
}

#[test]
fn a_workspace_whose_members_opt_in_passes() {
    let scratch = ScratchRepo::new();
    scratch.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\"]\n\n[workspace.lints.rust]\n\
         [workspace.lints.clippy]\n",
    );
    scratch.write(
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\n\n[lints]\nworkspace = true\n",
    );

    let problems = lint_opt_in(&Repo::new(scratch.path()), None);
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn the_lints_are_set_at_their_levels() {
    let root = root_manifest(&meta::repo().read("Cargo.toml"));
    let problems = lint_level_problems(&root, LEVELS);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Negative control: a lint left out, one lowered to `warn`, and the `{ level = … }` form at the
/// right level, which passes.
#[test]
fn a_lowered_or_missing_lint_level_is_refused() {
    let root = root_manifest(
        "[workspace.lints.rust]\nunsafe_code = { level = \"forbid\" }\n\n\
         [workspace.lints.clippy]\nunwrap_used = \"warn\"\n",
    );
    assert!(root_lint_problems(&root).is_empty());
    let required = [
        LintLevel {
            table: "rust",
            lint: "unsafe_code",
            level: "forbid",
        },
        LintLevel {
            table: "clippy",
            lint: "unwrap_used",
            level: "deny",
        },
        LintLevel {
            table: "clippy",
            lint: "expect_used",
            level: "deny",
        },
    ];
    assert_eq!(
        strings(&lint_level_problems(&root, &required)),
        [
            r#"[workspace.lints.clippy] sets unwrap_used = "warn"; it must be "deny""#,
            r#"[workspace.lints.clippy] does not set expect_used = "deny""#,
        ]
    );
}

#[test]
fn nothing_outside_the_manifest_changes_a_lint_level() {
    let repo = meta::repo();
    let found = repo.tracked_or_new(&["*clippy.toml", ".cargo/config", ".cargo/config.toml"]);
    assert_eq!(
        found,
        ["clippy.toml"],
        "unexpected clippy or Cargo configuration files"
    );
    let mut sources = vec!["Makefile".to_owned()];
    sources.extend(repo.tracked_or_new(&[".mk/*.mk"]));
    assert!(
        sources.iter().any(|path| path == ".mk/rust.mk"),
        "the scan saw no make fragment, not even .mk/rust.mk: {sources:?}"
    );
    let tokens = override_tokens();
    let tokens: Vec<&str> = tokens.iter().map(String::as_str).collect();
    let problems = override_problems(&repo.read_all(&sources), &tokens);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Negative control: a make fragment that caps the lints, lowers a group or one of the levels, or
/// points clippy elsewhere is refused; a plain one passes.
#[test]
fn a_lint_override_outside_the_manifest_is_refused() {
    let tokens = override_tokens();
    let tokens: Vec<&str> = tokens.iter().map(String::as_str).collect();
    for text in [
        "lint:\n\tcargo clippy -- --cap-lints warn\n",
        "export CLIPPY_CONF_DIR := /elsewhere\n",
        "RUSTFLAGS += -A clippy::pedantic\n",
        "RUSTFLAGS += -A clippy::unwrap_used\n",
    ] {
        let files = [(".mk/x.mk".to_owned(), text.to_owned())];
        assert_eq!(override_problems(&files, &tokens).len(), 1, "{text}");
    }
    let plain = [(".mk/x.mk".to_owned(), "lint:\n\tcargo clippy\n".to_owned())];
    assert_eq!(override_problems(&plain, &tokens), []);
}
