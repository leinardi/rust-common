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
//! gate. The check is `meta::workspace::layout_problems`; the negative control runs it on a
//! scratch tree.

use std::fs;
use std::path::Path;

use meta::repo;
use meta::workspace::layout_problems;

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| panic!("create {relative}: {error}"));
    }
    fs::write(&path, content).unwrap_or_else(|error| panic!("write {relative}: {error}"));
}

#[test]
fn members_are_meta_and_the_tracked_crates() {
    let problems = layout_problems(&repo::root(), &repo::tracked_crates());
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn layout_check_is_not_blind() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"meta\", \"tools/xtask\", \"crates/alpha\", \"crates/gone\", \
         \"crates/alpha/nested\"]\n",
    );

    assert_eq!(
        layout_problems(root, &["alpha".to_owned(), "beta".to_owned()]),
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
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\", \"meta\"]\n",
    );

    let problems = layout_problems(root, &["alpha".to_owned()]);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
