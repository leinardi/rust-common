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

//! Every workspace member is the meta crate or a library crate under `crates/<name>`, and every
//! tracked `crates/*/Cargo.toml` is a member: a member elsewhere would escape the feature matrix
//! and the feature-set coverage check, and a tracked crate that is not a member would escape every
//! gate. The check is `repo_meta::workspace::layout_problems`; the negative control runs it on a
//! scratch tree.

use repo_meta::scratch::ScratchRepo;
use repo_meta::workspace::{layout_problems, tracked_crates};
use repo_meta::{report, strings};

#[test]
fn members_are_meta_and_the_tracked_crates() {
    let repo = meta::repo();
    let problems = layout_problems(&repo, "crates", &["meta"], &tracked_crates(&repo, "crates"));
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn layout_check_is_not_blind() {
    let scratch = ScratchRepo::new();
    scratch.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"meta\", \"tools/xtask\", \"crates/alpha\", \"crates/gone\", \
         \"crates/alpha/nested\"]\n",
    );
    scratch.write("crates/alpha/Cargo.toml", "[package]\nname = \"alpha\"\n");
    scratch.write("crates/beta/Cargo.toml", "[package]\nname = \"beta\"\n");
    scratch.write(
        "crates/untracked/Cargo.toml",
        "[package]\nname = \"untracked\"\n",
    );
    scratch.git(&["add", "Cargo.toml", "crates/alpha", "crates/beta"]);
    let repo = scratch.repo();

    let crates = tracked_crates(&repo, "crates");
    assert_eq!(crates, ["alpha", "beta"]);
    assert_eq!(
        strings(&layout_problems(&repo, "crates", &["meta"], &crates)),
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
    let scratch = ScratchRepo::new();
    scratch.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\", \"meta\"]\n",
    );

    let problems = layout_problems(&scratch.repo(), "crates", &["meta"], &["alpha".to_owned()]);
    assert!(problems.is_empty(), "{}", report(&problems));
}
