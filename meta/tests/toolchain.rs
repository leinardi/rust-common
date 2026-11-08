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

//! One Rust release line and one ShellCheck everywhere they are written: `rust-toolchain.toml`
//! pins an exact release and the workspace's `rust-version` is that release's line, and the
//! ShellCheck the shellcheck-py hook wraps is the one CI's setup-shellcheck step installs. The
//! pins are `repo_meta::toolchain::one_value`'s; the negative controls run it on altered copies.
//!
//! CI installs the toolchain from `rust-toolchain.toml` itself, so no workflow repeats the
//! release.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/toolchain.rs at c9e79afa8810 and
//! swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417, without the image and release
//! pins, since this repository builds no image.

use repo_meta::strings;
use repo_meta::toolchain::{PinSource, one_value};
use repo_meta::{Problem, report};

const TOOLCHAIN: &str = "rust-toolchain.toml";
const MANIFEST: &str = "Cargo.toml";
const HOOKS: &str = ".pre-commit-config.yaml";
/// The workflow whose gh-composite-actions setup-shellcheck step names the version it installs.
const SHELLCHECK_WORKFLOW: &str = ".github/workflows/ci.yaml";

/// The Rust line in the toolchain file and in the manifest.
fn rust_line(toolchain: &str, manifest: &str) -> Vec<Problem> {
    one_value(
        "the Rust line",
        &[
            PinSource::rust_channel_line(TOOLCHAIN, toolchain),
            PinSource::rust_version_line(MANIFEST, manifest),
        ],
        &[],
    )
}

/// The ShellCheck release of the hook (its rev adds a fourth component for the wrapper, so only
/// the first three count) and of the CI step that installs it.
fn shellcheck(hooks: &str, workflow: &str) -> Vec<Problem> {
    one_value(
        "ShellCheck",
        &[
            PinSource::new(
                HOOKS,
                hooks,
                r"repo: https://github\.com/shellcheck-py/shellcheck-py\s+rev:\s*(v\d+\.\d+\.\d+)",
            ),
            PinSource::new(
                SHELLCHECK_WORKFLOW,
                workflow,
                r#"/setup-shellcheck@[0-9a-f]{40}[^\n]*\n\s+with:\n\s+version:\s*["']?(v\d+\.\d+\.\d+)"#,
            ),
        ],
        &[],
    )
}

#[test]
fn rust_version_is_the_toolchain_line() {
    let repo = meta::repo();
    let problems = rust_line(&repo.read(TOOLCHAIN), &repo.read(MANIFEST));
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn shellcheck_is_one_version() {
    let repo = meta::repo();
    let problems = shellcheck(&repo.read(HOOKS), &repo.read(SHELLCHECK_WORKFLOW));
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Negative control: a toolchain bumped to another line, a manifest left on the old one, and a
/// channel that is no release; the line and the exact release as a `rust-version` pass.
#[test]
fn a_rust_version_off_the_line_is_refused() {
    let toolchain = "[toolchain]\nchannel = \"1.98.1\"\n";
    let manifest = |version: &str| format!("[workspace.package]\nrust-version = \"{version}\"\n");
    assert_eq!(rust_line(toolchain, &manifest("1.98")), []);
    assert_eq!(rust_line(toolchain, &manifest("1.98.1")), []);
    assert_eq!(
        strings(&rust_line(toolchain, &manifest("1.97"))),
        ["the Rust line is not one value: 1.97 in Cargo.toml; 1.98 in rust-toolchain.toml"]
    );
    assert_eq!(
        strings(&rust_line(
            &toolchain.replace("1.98.1", "1.99.0"),
            &manifest("1.98")
        )),
        ["the Rust line is not one value: 1.98 in Cargo.toml; 1.99 in rust-toolchain.toml"]
    );
    for channel in ["stable", "1.98"] {
        let toolchain = format!("[toolchain]\nchannel = \"{channel}\"\n");
        assert_eq!(
            rust_line(&toolchain, &manifest("1.98")).len(),
            1,
            "{channel}"
        );
    }
    assert_eq!(
        rust_line(toolchain, "[package]\n").len(),
        1,
        "no rust-version"
    );
}

/// Negative control: the hook and the CI step on different ShellCheck releases.
#[test]
fn two_shellcheck_versions_are_refused() {
    let hooks =
        "repos:\n  - repo: https://github.com/shellcheck-py/shellcheck-py\n    rev: v0.11.0.1\n";
    let step = |version: &str| {
        format!(
            "      - uses: leinardi/gh-composite-actions/setup-shellcheck@{} # v1.0.0\n        \
             with:\n          version: {version}\n",
            "0".repeat(40)
        )
    };
    assert_eq!(shellcheck(hooks, &step("v0.11.0")), []);
    assert_eq!(
        strings(&shellcheck(hooks, &step("v0.10.0"))),
        [
            "ShellCheck is not one value: v0.10.0 in .github/workflows/ci.yaml; \
             v0.11.0 in .pre-commit-config.yaml"
        ]
    );
}
