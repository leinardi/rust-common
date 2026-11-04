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

//! One Rust release and one ShellCheck everywhere they are written: `rust-toolchain.toml` pins an
//! exact release, the workspace's `rust-version` is that release's line, every CI step that sets
//! up Rust installs exactly that release, and the ShellCheck the shellcheck-py hook wraps is the
//! one CI's setup-shellcheck action installs. The checks are `repo_meta::toolchain`'s; the
//! negative controls run them on altered copies.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/toolchain.rs at c9e79afa8810 and
//! swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417, without the image and release
//! pins, since this repository builds no image.

use repo_meta::toolchain::{
    RustVersion, ci_toolchain_problems, rust_release, rust_version_problems, shellcheck_problems,
};
use repo_meta::{Problem, report, strings, yaml};

const TOOLCHAIN: &str = "rust-toolchain.toml";
const HOOKS: &str = ".pre-commit-config.yaml";
const SHELLCHECK_ACTION: &str = ".github/actions/setup-shellcheck/action.yaml";

/// The release `rust-toolchain.toml` pins.
fn release() -> String {
    rust_release(TOOLCHAIN, &meta::repo().read(TOOLCHAIN))
        .unwrap_or_else(|problem| panic!("{problem}"))
}

/// Every workflow and composite action, parsed.
fn ci_documents() -> Vec<(String, yaml::Value)> {
    let repo = meta::repo();
    let files = repo.files_under(".github", &[".yaml", ".yml"]);
    assert!(
        files.iter().any(|file| file == ".github/workflows/ci.yaml"),
        "the scan saw no CI workflow: {files:?}"
    );
    files
        .into_iter()
        .map(|file| {
            let document =
                yaml::parse(&file, &repo.read(&file)).unwrap_or_else(|problem| panic!("{problem}"));
            (file, document)
        })
        .collect()
}

fn workflow(file: &str, content: &str) -> (String, yaml::Value) {
    let document = yaml::parse(file, content).unwrap_or_else(|problem| panic!("{problem}"));
    (file.to_owned(), document)
}

#[test]
fn rust_version_is_the_toolchain_line() {
    let problems = rust_version_problems(
        "Cargo.toml",
        &meta::repo().read("Cargo.toml"),
        &release(),
        RustVersion::Line,
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn ci_installs_the_toolchain_release() {
    let problems = ci_toolchain_problems(&ci_documents(), &release());
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn shellcheck_is_one_version() {
    let repo = meta::repo();
    let problems = shellcheck_problems(
        HOOKS,
        &repo.read(HOOKS),
        SHELLCHECK_ACTION,
        &repo.read(SHELLCHECK_ACTION),
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Negative control: a channel or a release without its patch is no pin.
#[test]
fn a_channel_is_not_a_release() {
    assert_eq!(
        rust_release(TOOLCHAIN, "[toolchain]\nchannel = \"1.98.1\"\n"),
        Ok("1.98.1".to_owned())
    );
    for channel in ["stable", "1.98"] {
        let toolchain = format!("[toolchain]\nchannel = \"{channel}\"\n");
        assert!(
            rust_release(TOOLCHAIN, &toolchain).is_err(),
            "{channel} read as a release"
        );
    }
    assert_eq!(
        rust_release(TOOLCHAIN, "[toolchain]\n"),
        Err(Problem::at(TOOLCHAIN, "names no channel"))
    );
}

/// Negative control: another line, no rust-version at all; the line and the exact release pass.
#[test]
fn a_rust_version_off_the_line_is_refused() {
    let check = |manifest: &str| {
        strings(&rust_version_problems(
            "Cargo.toml",
            manifest,
            "1.98.1",
            RustVersion::Line,
        ))
    };
    assert_eq!(check("rust-version = \"1.98\"\n"), Vec::<String>::new());
    assert_eq!(check("rust-version = \"1.98.1\"\n"), Vec::<String>::new());
    assert_eq!(
        check("rust-version = \"1.97\"\n"),
        [
            r#"Cargo.toml: rust-version is "1.97", not the line 1.98 of rust-toolchain.toml's 1.98.1"#
        ]
    );
    assert_eq!(check("[package]\n"), ["Cargo.toml: sets no rust-version"]);
}

/// Negative control: a step that installs another toolchain or a channel is refused, and a
/// workflow set that sets up no Rust at all is a problem too.
#[test]
fn another_ci_toolchain_is_refused() {
    let step = |toolchain: &str| {
        format!(
            "jobs:\n  rust:\n    steps:\n      - uses: \
             dtolnay/rust-toolchain@683b588c487a69d01f6c6ddf40f036ff682cc2c8\n        with:\n          \
             toolchain: {toolchain}\n"
        )
    };
    let file = ".github/workflows/ci.yaml";
    assert_eq!(
        ci_toolchain_problems(&[workflow(file, &step("1.98.1"))], "1.98.1"),
        []
    );
    for toolchain in ["stable", "1.98", "1.97.0"] {
        let problems = ci_toolchain_problems(&[workflow(file, &step(toolchain))], "1.98.1");
        assert_eq!(problems.len(), 1, "{toolchain}: {problems:?}");
    }
    let none = workflow(file, "jobs:\n  lint:\n    steps:\n      - run: make lint\n");
    assert!(!ci_toolchain_problems(&[none], "1.98.1").is_empty());
}

/// Negative control: the hook and the action on different ShellCheck releases.
#[test]
fn two_shellcheck_versions_are_refused() {
    let hooks =
        "repos:\n  - repo: https://github.com/shellcheck-py/shellcheck-py\n    rev: v0.11.0.1\n";
    let action = |version: &str| format!("      env:\n        SHELLCHECK_VERSION: {version}\n");
    assert_eq!(
        shellcheck_problems(HOOKS, hooks, SHELLCHECK_ACTION, &action("v0.11.0")),
        []
    );
    assert!(!shellcheck_problems(HOOKS, hooks, SHELLCHECK_ACTION, &action("v0.10.0")).is_empty());
}
