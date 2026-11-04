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

//! The gate of each project: found, missing and skipped, missing and required, and the tool list
//! held closed.
//!
//! Extracted from bitgauge/testenv/src/toolgate.rs at 600fc3905200 (`lookup_finds_or_gates`).

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::{Gate, Locate, Note};

const AWTRIX: Gate = Gate {
    require_env: "AWTRIX_CONTROLLER_REQUIRE_TOOLS",
    tools: &["droast", "shellcheck"],
    locate: Locate::Path,
    note: Note::Stderr,
};

const BITGAUGE: Gate = Gate {
    require_env: "BITGAUGE_REQUIRE_TOOLS",
    tools: &["eslint", "fallow"],
    locate: Locate::Ancestors {
        dir: "web/node_modules/.bin",
        hint: "run npm ci in web/",
    },
    note: Note::Captured,
};

fn write_file(path: &Path, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// bitgauge's gate: a tool is found under the nearest `web/node_modules/.bin`, a missing one is
/// `None` unless required, and then it is an error naming the variable and the install hint.
#[test]
fn ancestors_find_or_gate() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a/b");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(
        BITGAUGE.lookup_in("eslint", &nested, None, false).unwrap(),
        None
    );
    let eslint = dir.path().join("web/node_modules/.bin/eslint");
    // Any mode: npm's links are what they are.
    write_file(&eslint, 0o644);
    assert_eq!(
        BITGAUGE.lookup_in("eslint", &nested, None, true).unwrap(),
        Some(eslint.clone())
    );
    // The nearest ancestor wins.
    let nearer = dir.path().join("a/web/node_modules/.bin/eslint");
    write_file(&nearer, 0o755);
    assert_eq!(
        BITGAUGE.lookup_in("eslint", &nested, None, true).unwrap(),
        Some(nearer)
    );
    let error = BITGAUGE
        .lookup_in("fallow", &nested, None, true)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "fallow is required (BITGAUGE_REQUIRE_TOOLS=1) but was not found: \
         web/node_modules/.bin/fallow: not installed; run npm ci in web/"
    );
}

/// awtrix-controller's and swarm-rootless-docker's gate: an executable file on `PATH`, the first
/// directory winning; a file that is not executable, or a directory, does not count.
#[test]
fn path_finds_executables_only() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    write_file(&first.join("droast"), 0o644);
    fs::create_dir_all(first.join("shellcheck")).unwrap();
    write_file(&second.join("droast"), 0o755);
    let path = env_path(&[&first, &second]);
    assert_eq!(
        AWTRIX
            .lookup_in("droast", dir.path(), Some(path.clone()), true)
            .unwrap(),
        Some(second.join("droast"))
    );
    write_file(&first.join("droast"), 0o755);
    assert_eq!(
        AWTRIX
            .lookup_in("droast", dir.path(), Some(path.clone()), true)
            .unwrap(),
        Some(first.join("droast"))
    );
    let error = AWTRIX
        .lookup_in("shellcheck", dir.path(), Some(path.clone()), true)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "shellcheck is required (AWTRIX_CONTROLLER_REQUIRE_TOOLS=1) but was not found on PATH"
    );
    assert_eq!(
        AWTRIX
            .lookup_in("shellcheck", dir.path(), Some(path), false)
            .unwrap(),
        None
    );
    // No PATH at all finds nothing.
    assert_eq!(
        AWTRIX.lookup_in("droast", dir.path(), None, false).unwrap(),
        None
    );
}

/// A tool off the list is refused whatever the environment: the list is closed.
#[test]
#[should_panic(
    expected = r#"toolgate: "hadolint" is not one of the gated tools ["droast", "shellcheck"]"#
)]
fn a_tool_off_the_list_is_refused() {
    let _ = AWTRIX.lookup_in("hadolint", Path::new("/"), None, false);
}

/// The macro names the caller's manifest directory's parent: here, the workspace's `crates`.
#[test]
fn repo_root_is_the_caller_s() {
    let root = crate::repo_root!();
    assert!(
        root.join("testenv-core/Cargo.toml").is_file(),
        "{}",
        root.display()
    );
}

fn env_path(dirs: &[&Path]) -> OsString {
    std::env::join_paths(dirs).unwrap()
}
