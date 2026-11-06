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

//! `buildinfo!` reads the stamps of the crate that calls it. A scratch program depending on this
//! crate by path is built with Cargo, its build script setting `PROBE_VERSION` and an empty
//! `PROBE_COMMIT` through `cargo:rustc-env`, which reaches that program's compilation only: had
//! the `option_env!` reads expanded in this crate, they would see nothing and give the defaults.
//! The variables are also removed from the environment Cargo itself runs in, so they come from the
//! build script alone.
//!
//! Written for this crate.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAIN: &str = r#"
use service_kit::{BuildInfo, Defaults, EmptyStamp, buildinfo};

const KEPT: BuildInfo = buildinfo!(
    "PROBE",
    Defaults { version: "dev", commit: "none", date: "unknown", empty: EmptyStamp::Kept },
);

const UNSTAMPED: BuildInfo = buildinfo!(
    "PROBE",
    Defaults { version: "dev", commit: "unknown", date: "unknown", empty: EmptyStamp::Unstamped },
);

fn main() {
    for build in [KEPT, UNSTAMPED] {
        println!("{}|{}|{}", build.version, build.commit, build.date);
    }
}
"#;

const BUILD_SCRIPT: &str = r#"
fn main() {
    println!("cargo:rustc-env=PROBE_VERSION=v9.8.7-probe");
    println!("cargo:rustc-env=PROBE_COMMIT=");
}
"#;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| panic!("{}: {error}", parent.display()));
    }
    fs::write(path, contents).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

#[test]
fn buildinfo_expands_in_the_calling_crate() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("buildinfo-probe");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    write(
        &scratch.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"buildinfo-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             publish = false\n\n\
             [dependencies]\nservice-kit = {{ path = {manifest_dir:?}, default-features = false }}\n\n\
             # Not a member of the rust-common workspace above it.\n[workspace]\n"
        ),
    );
    write(&scratch.join("build.rs"), BUILD_SCRIPT);
    write(&scratch.join("src/main.rs"), MAIN);

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["run", "--quiet", "--offline", "--manifest-path"])
        .arg(scratch.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", scratch.join("target"))
        .env_remove("PROBE_VERSION")
        .env_remove("PROBE_COMMIT")
        .env_remove("PROBE_DATE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo run: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        // Kept: the empty commit stays empty. Unstamped: it takes the default. Both: the unset
        // date takes the default.
        "v9.8.7-probe||unknown\nv9.8.7-probe|unknown|unknown\n"
    );
}
