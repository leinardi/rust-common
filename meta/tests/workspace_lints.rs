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
//! The check is `repo_meta::workspace`'s; the negative controls run it on scratch trees.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/workspace_lints.rs at c9e79afa8810.

use repo_meta::scratch::ScratchRepo;
use repo_meta::workspace::lint_opt_in;
use repo_meta::{report, strings};

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
        strings(&lint_opt_in(&scratch.repo(), None)),
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

    let problems = lint_opt_in(&scratch.repo(), None);
    assert!(problems.is_empty(), "{}", report(&problems));
}
