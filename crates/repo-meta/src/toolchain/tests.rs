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

//! The pin readings' negative controls: `one_value` needs every source and one value, the Rust
//! release is read exactly, each preset reads its file's form of the release, and a bump in one
//! file only is found.
//!
//! Extracted from swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417 and
//! bitgauge/meta/tests/toolchain.rs at 600fc3905200.

use super::{PinSource, PinValue, make_value, one_value, rust_release, toolchain_targets};
use crate::strings;

const BG_DOCKERFILE: &str = include_str!("../../tests/testdata/bitgauge.Dockerfile");

#[test]
fn one_value_needs_every_source_and_one_value() {
    let sources = [
        PinSource::new("a", "X=1\n", r"(?m)^X=(\S+)$"),
        PinSource::new("b", "X=1\nX=1\n", r"(?m)^X=(\S+)$"),
        PinSource::new("c", "nothing\n", r"(?m)^X=(\S+)$").optional(),
    ];
    assert_eq!(one_value("x", &sources, &[PinValue::new("1", "ci")]), []);
    assert_eq!(
        strings(&one_value("x", &sources, &[PinValue::new("2", "ci")])),
        ["x is not one value: 1 in a, b; 2 in ci"]
    );
    let missing = [
        PinSource::new("a", "nothing\n", r"(?m)^X=(\S+)$"),
        PinSource {
            content: None,
            ..PinSource::new("b", "", r"(?m)^X=(\S+)$")
        },
    ];
    assert_eq!(
        strings(&one_value("x", &missing, &[])),
        [
            r"x: a does not write it ((?m)^X=(\S+)$)",
            "x: b cannot be read"
        ]
    );
    let optional = [PinSource::new("a", "nothing\n", r"(?m)^X=(\S+)$").optional()];
    assert_eq!(
        strings(&one_value("x", &optional, &[])),
        ["x: no source writes it; the check would pass on nothing"]
    );
    assert_eq!(
        strings(&one_value("x", &[], &[])),
        ["x: no source and no value; the check would pass on nothing"]
    );
}

#[test]
fn the_release_is_exact() {
    assert_eq!(
        rust_release("rust-toolchain.toml", "[toolchain]\nchannel = \"1.98.1\"\n").unwrap(),
        "1.98.1"
    );
    for channel in ["stable", "1.98", "nightly-2026-10-01", "1.98.1-x86_64"] {
        assert!(
            rust_release("t", &format!("[toolchain]\nchannel = \"{channel}\"\n")).is_err(),
            "{channel} was accepted as an exact release"
        );
    }
    assert!(rust_release("t", "[toolchain]\n").is_err());
    assert_eq!(
        toolchain_targets(
            "[toolchain]\nchannel = \"1.98.1\"\ntargets = [\"x86_64-unknown-linux-musl\"]\n"
        ),
        ["x86_64-unknown-linux-musl"]
    );
    assert!(toolchain_targets("not toml [").is_empty());
}

#[test]
fn make_values_follow_make() {
    for (text, want) in [
        ("X := 1\n", Some("1")),
        ("X := 1\nX = 2\n", Some("2")),
        ("X := 1\nX ?= 2\n", Some("1")),
        ("X ?= 2\n", Some("2")),
        ("X := 1\nX += 2\n", Some("1 2")),
        ("X := 1\noverride X := 3\n", Some("3")),
        ("export X := 4\n", Some("4")),
        ("X != echo 5\n", Some("!=echo 5")),
        ("# X := 1\nXY := 2\n", None),
    ] {
        assert_eq!(make_value(text, "X").as_deref(), want, "{text:?}");
    }
}

const TOOLCHAIN: &str = "[toolchain]\nchannel = \"1.98.1\"\n";
const MANIFEST: &str = "[workspace.package]\nrust-version = \"1.98\"\n";
const DOCKERFILE: &str = "ARG RUST_VERSION=1.98.1\n\
                          FROM --platform=$BUILDPLATFORM dhi.io/rust:1.98.1-debian13-dev AS build\n\
                          FROM dhi.io/static:20250419 AS runtime\n";

/// The presets read the release as each file writes it.
#[test]
fn the_presets_read_each_form_of_the_release() {
    let read = |source: &PinSource| -> Vec<String> {
        source
            .pattern
            .captures_iter(source.content.as_deref().unwrap())
            .map(|captures| captures[1].to_owned())
            .collect()
    };
    assert_eq!(read(&PinSource::rust_channel("t", TOOLCHAIN)), ["1.98.1"]);
    assert_eq!(
        read(&PinSource::rust_channel_line("t", TOOLCHAIN)),
        ["1.98"]
    );
    assert_eq!(read(&PinSource::rust_version("c", MANIFEST)), ["1.98"]);
    for manifest in ["rust-version = \"1.98\"\n", "rust-version = \"1.98.1\"\n"] {
        assert_eq!(read(&PinSource::rust_version_line("c", manifest)), ["1.98"]);
    }
    assert_eq!(
        read(&PinSource::rust_image_tag("d", DOCKERFILE)),
        ["1.98.1"]
    );
    assert_eq!(
        read(&PinSource::rust_version_arg("d", DOCKERFILE)),
        ["1.98.1"]
    );
    let bitgauge = read(&PinSource::rust_image_tag("d", BG_DOCKERFILE));
    assert!(
        !bitgauge.is_empty()
            && bitgauge
                .iter()
                .all(|release| release.split('.').count() == 3),
        "every dhi.io/rust tag of bitgauge's Dockerfile is a release: {bitgauge:?}"
    );
    // A channel that is not a release writes no line.
    for channel in ["stable", "1.98", "nightly-2026-10-01"] {
        let toolchain = format!("[toolchain]\nchannel = \"{channel}\"\n");
        let problems = one_value(
            "the line",
            &[PinSource::rust_channel_line("t", &toolchain)],
            &[],
        );
        assert_eq!(problems.len(), 1, "{channel}: {problems:?}");
    }
}

/// Negative control: the release line is one value across the toolchain file and the manifest,
/// and the exact release one across the toolchain file and the Dockerfile; moving one copy is
/// found.
#[test]
fn a_bump_in_one_file_only_is_found() {
    let line = |toolchain: &str, manifest: &str| {
        strings(&one_value(
            "the Rust line",
            &[
                PinSource::rust_channel_line("rust-toolchain.toml", toolchain),
                PinSource::rust_version_line("Cargo.toml", manifest),
            ],
            &[],
        ))
    };
    assert_eq!(line(TOOLCHAIN, MANIFEST), Vec::<String>::new());
    assert_eq!(
        line(&TOOLCHAIN.replace("1.98.1", "1.99.0"), MANIFEST),
        ["the Rust line is not one value: 1.98 in Cargo.toml; 1.99 in rust-toolchain.toml"]
    );
    assert_eq!(
        line(TOOLCHAIN, &MANIFEST.replace("1.98", "1.97")),
        ["the Rust line is not one value: 1.97 in Cargo.toml; 1.98 in rust-toolchain.toml"]
    );

    let release = |toolchain: &str, dockerfile: &str| {
        strings(&one_value(
            "the Rust release",
            &[
                PinSource::rust_channel("rust-toolchain.toml", toolchain),
                PinSource::rust_image_tag("Dockerfile", dockerfile),
                PinSource::rust_version_arg("Dockerfile", dockerfile),
            ],
            &[],
        ))
    };
    assert_eq!(release(TOOLCHAIN, DOCKERFILE), Vec::<String>::new());
    assert_eq!(
        release(TOOLCHAIN, &DOCKERFILE.replace("rust:1.98.1", "rust:1.98.0")),
        [
            "the Rust release is not one value: 1.98.0 in Dockerfile; 1.98.1 in rust-toolchain.toml, Dockerfile"
        ]
    );
    assert_eq!(
        release(
            TOOLCHAIN,
            &DOCKERFILE.replace("RUST_VERSION=1.98.1", "RUST_VERSION=1.98.0")
        ),
        [
            "the Rust release is not one value: 1.98.0 in Dockerfile; 1.98.1 in rust-toolchain.toml, Dockerfile"
        ]
    );
    // A Dockerfile that drops its pin is a problem, not a pass on one source fewer.
    assert_eq!(
        strings(&one_value(
            "the Rust release",
            &[
                PinSource::rust_channel("rust-toolchain.toml", TOOLCHAIN),
                PinSource::rust_version_arg("Dockerfile", "FROM scratch\n"),
            ],
            &[],
        ))
        .len(),
        1
    );
}
