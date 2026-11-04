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

//! `feature-sets.toml` covers the crates: every crate under `crates/` has an entry with a
//! `--no-default-features` set and an `--all-features` set, and every feature it declares is named
//! in a set, so `make rust-feature-matrix` builds each feature in a set a consumer actually uses.
//! The negative controls run the same checks over scratch trees, so the check on this repository
//! is not trusted on the strength of having found nothing to check.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

use meta::feature_sets::{coverage_problems, parse, plan, run};
use meta::repo;

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| panic!("create {relative}: {error}"));
    }
    fs::write(&path, content).unwrap_or_else(|error| panic!("write {relative}: {error}"));
}

const ALPHA_MANIFEST: &str = r#"
[package]
name = "alpha"
version = "0.1.0"

[features]
default = ["a"]
a = []
b = ["dep:hidden"]

[dependencies]
hidden = { version = "1", optional = true }
implicit = { version = "1", optional = true }
always = "1"
"#;

#[test]
fn feature_sets_cover_every_crate_feature() {
    let problems = coverage_problems(&repo::root(), &repo::tracked_crates());
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn coverage_check_is_not_blind() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    write(root, "crates/alpha/Cargo.toml", ALPHA_MANIFEST);
    write(
        root,
        "crates/beta/Cargo.toml",
        "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "feature-sets.toml",
        r#"
alpha = [{ no-default-features = true }, { features = ["a", "typo"] }]
ghost = [{}]
"#,
    );

    let problems = coverage_problems(root, &["alpha".to_owned(), "beta".to_owned()]);
    assert_eq!(
        problems,
        [
            "feature-sets.toml lists ghost, which is not a crate under crates/",
            "feature-sets.toml: alpha has no { all-features = true } set",
            r#"feature-sets.toml: alpha declares feature "b", which no set names"#,
            r#"feature-sets.toml: alpha declares feature "implicit", which no set names"#,
            r#"feature-sets.toml: alpha names feature "typo", which crates/alpha/Cargo.toml does not declare"#,
            "feature-sets.toml has no entry for crate beta",
        ]
    );
}

#[test]
fn a_complete_entry_passes() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    write(root, "crates/alpha/Cargo.toml", ALPHA_MANIFEST);
    write(
        root,
        "feature-sets.toml",
        r#"
alpha = [
    { no-default-features = true },
    { features = ["a", "b"] },
    { no-default-features = true, features = ["implicit"] },
    { all-features = true },
]
"#,
    );

    let problems = coverage_problems(root, &["alpha".to_owned()]);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn parse_refuses_sets_that_would_select_nothing_they_say() {
    for (text, want) in [
        ("alpha = []", "lists no feature set"),
        ("alpha = [{ no-default-feature = true }]", "unknown key"),
        (
            "alpha = [{ all-features = false }]",
            "all-features must be true",
        ),
        ("alpha = [{ features = [] }]", "features is empty"),
        (
            "alpha = [{ features = [\"a,b\"] }]",
            "is not a feature name",
        ),
        ("alpha = [{ features = [\"a\", \"a\"] }]", "listed twice"),
        ("alpha = [{}, {}]", "repeats the set"),
        (
            "alpha = [{ all-features = true, features = [\"a\"] }]",
            "combined with another key",
        ),
        ("\"../alpha\" = [{}]", "is not a crate directory name"),
        (
            "alpha = { features = [\"a\"] }",
            "is not an array of feature sets",
        ),
        ("alpha = [", "is not valid TOML"),
    ] {
        let problems = parse(text).expect_err(text);
        assert!(
            problems.iter().any(|problem| problem.contains(want)),
            "{text:?}: want a problem containing {want:?}, got {problems:?}"
        );
    }
}

#[test]
fn plan_lints_and_tests_one_package_at_a_time() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    write(root, "crates/alpha/Cargo.toml", ALPHA_MANIFEST);
    write(
        root,
        "feature-sets.toml",
        "alpha = [{ no-default-features = true, features = [\"a\", \"b\"] }, { all-features = true }]\n",
    );

    let commands: Vec<String> = plan(root)
        .expect("plan")
        .iter()
        .map(|args| args.join(" "))
        .collect();
    let target = "--manifest-path crates/alpha/Cargo.toml -p alpha";
    assert_eq!(
        commands,
        [
            format!(
                "clippy --locked --all-targets {target} --no-default-features --features a,b \
                 -- -D warnings"
            ),
            format!("test --locked {target} --no-default-features --features a,b"),
            format!("clippy --locked --all-targets {target} --all-features -- -D warnings"),
            format!("test --locked {target} --all-features"),
        ]
    );
}

#[test]
fn plan_refuses_a_crate_without_a_manifest() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    write(root, "crates/alpha/Cargo.toml", ALPHA_MANIFEST);
    write(root, "feature-sets.toml", "alpha = [{}]\nghost = [{}]\n");

    let problems = plan(root).expect_err("a listed crate without a manifest");
    assert_eq!(
        problems,
        ["feature-sets.toml lists a crate whose manifest crates/ghost/Cargo.toml does not exist"]
    );
}

#[test]
fn plan_refuses_a_missing_file_and_runs_nothing_for_an_empty_one() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();
    let problems = plan(root).expect_err("no feature-sets.toml");
    assert!(
        problems[0].starts_with("read feature-sets.toml: "),
        "{problems:?}"
    );

    write(root, "feature-sets.toml", "# no crate yet\n");
    assert_eq!(plan(root).expect("plan"), Vec::<Vec<String>>::new());
}

/// A scratch workspace whose one crate compiles cleanly with its default features, and leaves an
/// unused import behind without them.
fn warning_only_without_defaults(root: &Path, sets: &str) {
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\"]\nresolver = \"3\"\n",
    );
    write(
        root,
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [features]\ndefault = [\"fmt\"]\nfmt = []\n",
    );
    write(
        root,
        "crates/alpha/src/lib.rs",
        "//! Scratch.\n\nuse std::fmt::Write as _;\n\n/// Greets.\n///\n/// # Errors\n///\n\
         /// Never.\n#[cfg(feature = \"fmt\")]\n\
         pub fn greet(text: &mut String) -> std::fmt::Result {\n    write!(text, \"hello\")\n}\n",
    );
    write(root, "feature-sets.toml", sets);
}

fn run_quietly(root: &Path) -> (bool, String) {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut out = Vec::new();
    let mut err = Vec::new();
    let passed = run(root, &cargo, &mut out, &mut err, false)
        .unwrap_or_else(|error| panic!("run the matrix: {error}"));
    (passed, String::from_utf8_lossy(&err).into_owned())
}

/// The negative control for the matrix itself: a warning that only a subset of the features
/// produces fails that set, as plain `cargo build` would not.
#[test]
fn matrix_fails_on_a_warning_in_one_feature_set() {
    let scratch = tempfile::tempdir().expect("scratch directory");
    let root = scratch.path();

    warning_only_without_defaults(root, "alpha = [{}]\n");
    let (passed, err) = run_quietly(root);
    assert!(
        passed,
        "the default set is clean, yet the matrix failed: {err}"
    );

    warning_only_without_defaults(root, "alpha = [{}, { no-default-features = true }]\n");
    let (passed, err) = run_quietly(root);
    assert!(
        !passed,
        "the unused import without default features passed the matrix"
    );
    assert!(
        err.contains(
            "cargo clippy --locked --all-targets --manifest-path crates/alpha/Cargo.toml \
             -p alpha --no-default-features -- -D warnings failed"
        ),
        "{err}"
    );
}
