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

//! The pin checks' negative controls: each way to move one copy of a pin is found, and the
//! readings the rules rest on hold.
//!
//! Extracted from swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417
//! (`rust_step_rules`, `image_rust_rules`, `rust_line_reading`, `zigbuild_pin_rules`,
//! `rust_std_rules`, `hook_version_rules`, `trivy_rules`) and bitgauge/meta/tests/toolchain.rs at
//! 600fc3905200 (`rust_toolchain_ci_rules`, `rust_release_pin_control_fails_on_a_change`).

use super::make_value;
use super::{
    PinSource, PinValue, RUST_RELEASE_PINS, ReleasePins, RustVersion, ToolchainRef, ZigRecord,
    ci_rust_toolchains, ci_toolchain_problems, image_rust_problems, one_value,
    release_pin_problems, rust_release, rust_std_problems, rust_version_problems,
    shellcheck_problems, toolchain_targets, trivy_problems, zigbuild_action_problems,
};
use crate::{strings, yaml};

const BG_DOCKERFILE: &str = include_str!("../../tests/testdata/bitgauge.Dockerfile");
const BG_RELEASE_MK: &str = include_str!("../../tests/testdata/bitgauge.rust-release.mk");

fn documents(file: &str, content: &str) -> Vec<(String, yaml::Value)> {
    vec![(file.to_owned(), yaml::parse(file, content).unwrap())]
}

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
fn rust_version_follows_the_policy() {
    let manifest = |version: &str| format!("[workspace.package]\nrust-version = \"{version}\"\n");
    for (version, exact, line) in [
        ("1.98.1", true, true),
        ("1.98", false, true),
        ("1.98.0", false, true),
        ("1.97", false, false),
        ("1.98.x", false, false),
        ("1.9", false, false),
    ] {
        for (policy, pass) in [(RustVersion::Exact, exact), (RustVersion::Line, line)] {
            assert_eq!(
                rust_version_problems("Cargo.toml", &manifest(version), "1.98.1", policy)
                    .is_empty(),
                pass,
                "{version} under {policy:?}"
            );
        }
    }
    assert_eq!(
        strings(&rust_version_problems(
            "Cargo.toml",
            "[package]\n",
            "1.98.1",
            RustVersion::Line
        )),
        ["Cargo.toml: sets no rust-version"]
    );
}

/// Another release in the tag or in `RUST_VERSION`, a tag and an argument that differ, a floating
/// tag, a missing argument and a second build image are each refused; under the line policy
/// a later patch passes when the tag and the argument agree, and a tag outside a FROM is not read.
#[test]
fn image_rust_rules() {
    let good = "FROM --platform=$BUILDPLATFORM dhi.io/rust:1.98.1-debian13-dev AS build\n\
                ARG RUST_VERSION=1.98.1\n";
    let check = |sample: &str, policy| image_rust_problems("Dockerfile", sample, "1.98.1", policy);
    assert_eq!(check(good, RustVersion::Exact), []);
    assert_eq!(check(good, RustVersion::Line), []);
    for (name, sample) in [
        ("another tag", good.replace("rust:1.98.1-", "rust:1.98.0-")),
        (
            "another standard library",
            good.replace("RUST_VERSION=1.98.1", "RUST_VERSION=1.98.0"),
        ),
        ("a floating tag", good.replace("rust:1.98.1-", "rust:1-")),
        (
            "no RUST_VERSION",
            good.replace("ARG RUST_VERSION=1.98.1\n", ""),
        ),
        (
            "a second build image",
            format!("{good}FROM dhi.io/rust:1.98.1-dev\n"),
        ),
        (
            "the tag only in a comment",
            good.replace("FROM --platform", "# FROM --platform")
                + "FROM dhi.io/static:1 AS runtime\n",
        ),
    ] {
        assert!(
            !check(&sample, RustVersion::Exact).is_empty(),
            "{name} was not refused"
        );
    }
    let patched = good.replace("1.98.1", "1.98.2");
    assert_eq!(check(&patched, RustVersion::Line), []);
    assert_eq!(check(&patched, RustVersion::Exact).len(), 1);
    let split = good.replace("rust:1.98.1-", "rust:1.98.2-");
    assert_eq!(
        strings(&check(&split, RustVersion::Line)),
        [
            "Dockerfile: the dhi.io/rust tag's release 1.98.2 is not ARG RUST_VERSION's 1.98.1, \
             so the musl standard libraries are another release's"
        ]
    );
    assert_eq!(
        check(&good.replace("1.98.1", "1.97.0"), RustVersion::Line).len(),
        1,
        "another line"
    );
}

/// Another release, a channel, a release without its patch, and no toolchain at all are refused;
/// another action is not a toolchain step.
#[test]
fn ci_toolchain_rules() {
    let steps = documents(
        "sample.yaml",
        r#"jobs:
  ok:
    steps:
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567 # master
        with:
          toolchain: 1.98.1
  bad:
    steps:
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: 1.98.0
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: stable
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: "1.98"
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
      - uses: some/other-action@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: nightly
"#,
    );
    let problems = ci_toolchain_problems(&steps, "1.98.1", ToolchainRef::Input);
    assert_eq!(problems.len(), 4, "{problems:#?}");
    assert_eq!(
        strings(&ci_toolchain_problems(
            &documents("ci.yaml", "jobs:\n  a:\n    steps: []\n"),
            "1.98.1",
            ToolchainRef::Input
        )),
        ["no CI step sets up a Rust toolchain; the check would see nothing"]
    );
}

/// What reads as a toolchain: an action's input, its non-commit reference only where the
/// project reads one (`InputOrRef`), `RUSTUP_TOOLCHAIN`, and a `rustup` or `cargo +` command; an
/// action that names none is refused unless it reads `rust-toolchain.toml`.
#[test]
fn ci_toolchains_are_read_from_actions_and_scripts() {
    let documents = documents(
        "sample.yaml",
        r"jobs:
  rust:
    env:
      RUSTUP_TOOLCHAIN: nightly
    steps:
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567 # master
        with:
          toolchain: 1.98.1
      - uses: dtolnay/rust-toolchain@stable
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
      - uses: actions-rust-lang/setup-rust-toolchain@0123456789abcdef0123456789abcdef01234567
      - run: rustup toolchain install --no-self-update 1.97.0 && cargo +beta build
",
    );
    let no_toolchain = "sample.yaml: dtolnay/rust-toolchain names no toolchain, and does not read \
                        rust-toolchain.toml";
    let (values, problems) = ci_rust_toolchains(&documents, ToolchainRef::InputOrRef);
    let mut named: Vec<&str> = values.iter().map(|value| value.value.as_str()).collect();
    named.sort_unstable();
    assert_eq!(named, ["1.97.0", "1.98.1", "beta", "nightly", "stable"]);
    assert_eq!(strings(&problems), [no_toolchain]);
    let (values, problems) = ci_rust_toolchains(&documents, ToolchainRef::Input);
    let mut named: Vec<&str> = values.iter().map(|value| value.value.as_str()).collect();
    named.sort_unstable();
    assert_eq!(named, ["1.97.0", "1.98.1", "beta", "nightly"]);
    assert_eq!(strings(&problems), [no_toolchain, no_toolchain]);
}

/// `@<release>` with no `toolchain` input: read as the toolchain only under `InputOrRef`; under
/// `Input` it names none, even when the reference is the right release.
#[test]
fn a_release_reference_without_an_input() {
    let step = documents(
        "ci.yaml",
        "jobs:\n  rust:\n    steps:\n      - uses: dtolnay/rust-toolchain@1.98.1\n",
    );
    assert_eq!(
        ci_toolchain_problems(&step, "1.98.1", ToolchainRef::InputOrRef),
        []
    );
    assert_eq!(
        strings(&ci_toolchain_problems(&step, "1.98.1", ToolchainRef::Input)),
        [
            "ci.yaml: dtolnay/rust-toolchain names no toolchain, and does not read \
             rust-toolchain.toml"
        ]
    );
}

const DIGEST: &str = "1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026";
const ZIGBUILD_DIGEST: &str = "52be2cb85ad3875682564a29bac1832b2ab3db68b1c504765c92d5249cc9b96f";
const OTHER_DIGEST: &str = "9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8";

fn action() -> String {
    format!(
        "      env:\n        ZIG_VERSION: \"0.17.0\"\n        ZIG_SHA256: {DIGEST}\n        \
         CARGO_ZIGBUILD_VERSION: \"0.24.0\"\n        CARGO_ZIGBUILD_SHA256: {ZIGBUILD_DIGEST}\n"
    )
}

fn fragment() -> String {
    format!(
        "RUST_RELEASE_ZIG_VERSION := 0.17.0\nRUST_RELEASE_ZIG_SHA256_x86_64 := {DIGEST}\n\
         RUST_RELEASE_CARGO_ZIGBUILD_VERSION := 0.24.0\n\
         RUST_RELEASE_CARGO_ZIGBUILD_SHA256_x86_64-unknown-linux-musl := {ZIGBUILD_DIGEST}\n\
         RUST_RELEASE_RUST_STD_VERSION := 1.98.1\n\
         RUST_RELEASE_RUST_STD_SHA256_x86_64-unknown-linux-musl := {OTHER_DIGEST}\n"
    )
}

fn dockerfile() -> String {
    format!(
        "# syntax=docker/dockerfile:1.7@sha256:{}\nFROM dhi.io/rust:1.98.1-debian13-dev AS build\n\
         ARG ZIG_VERSION=0.17.0\nARG CARGO_ZIGBUILD_VERSION=0.24.0\nARG RUST_VERSION=1.98.1\n\
         RUN case x in amd64) zig_arch=x86_64; zig_sha256={DIGEST} ;; esac \\\n  \
         && case x in amd64) zigbuild_asset=x86_64-unknown-linux-musl; \
         zigbuild_sha256={ZIGBUILD_DIGEST} ;; esac \\\n  \
         && case x in amd64) triple=x86_64-unknown-linux-musl; std_sha256={OTHER_DIGEST} ;; esac\n",
        "0".repeat(64)
    )
}

/// A moved version or digest on either side, a pin the record does not write, and a pin written
/// twice with two values are each one problem; against either record.
#[test]
fn zigbuild_action_rules() {
    let fragment = fragment();
    let dockerfile = dockerfile();
    let records = |fragment: &str, dockerfile: &str| {
        [
            zigbuild_action_problems(
                "action.yaml",
                &action(),
                ZigRecord::Fragment {
                    file: "rust-release.mk",
                    content: fragment,
                },
            ),
            zigbuild_action_problems(
                "action.yaml",
                &action(),
                ZigRecord::Dockerfile {
                    file: "Dockerfile",
                    content: dockerfile,
                },
            ),
        ]
    };
    for problems in records(&fragment, &dockerfile) {
        assert_eq!(problems, []);
    }
    let commented = action().replace(
        "ZIG_VERSION: \"0.17.0\"",
        "ZIG_VERSION: \"0.17.0\" # the release make-common pins",
    );
    assert_eq!(
        zigbuild_action_problems(
            "action.yaml",
            &commented,
            ZigRecord::Fragment {
                file: "rust-release.mk",
                content: &fragment,
            },
        ),
        [],
        "a trailing comment is read past"
    );
    for (name, action) in [
        (
            "a moved zig in the action",
            action().replace("ZIG_VERSION: \"0.17.0\"", "ZIG_VERSION: \"0.16.0\""),
        ),
        (
            "another cargo-zigbuild digest in the action",
            action().replace(ZIGBUILD_DIGEST, OTHER_DIGEST),
        ),
    ] {
        let problems = zigbuild_action_problems(
            "action.yaml",
            &action,
            ZigRecord::Fragment {
                file: "rust-release.mk",
                content: &fragment,
            },
        );
        assert_eq!(problems.len(), 1, "{name}: {problems:#?}");
    }
    for (name, fragment, dockerfile) in [
        (
            "a moved cargo-zigbuild",
            fragment.replace("ZIGBUILD_VERSION := 0.24.0", "ZIGBUILD_VERSION := 0.23.0"),
            dockerfile.replace("ZIGBUILD_VERSION=0.24.0", "ZIGBUILD_VERSION=0.23.0"),
        ),
        (
            "another zig digest",
            fragment.replace(DIGEST, OTHER_DIGEST),
            dockerfile.replace(
                &format!("zig_sha256={DIGEST}"),
                &format!("zig_sha256={OTHER_DIGEST}"),
            ),
        ),
        (
            "no zig version",
            fragment.replace("RUST_RELEASE_ZIG_VERSION := 0.17.0\n", ""),
            dockerfile.replace("ARG ZIG_VERSION=0.17.0\n", ""),
        ),
        (
            "zig written twice",
            format!("{fragment}RUST_RELEASE_ZIG_VERSION := 0.16.0\n"),
            format!("{dockerfile}ARG ZIG_VERSION=0.16.0\n"),
        ),
    ] {
        for problems in records(&fragment, &dockerfile) {
            assert_eq!(problems.len(), 1, "{name}: {problems:#?}");
        }
    }
}

fn release_check(dockerfile: &str, fragment: &str) -> Vec<String> {
    strings(&release_pin_problems(
        "Dockerfile",
        dockerfile,
        ".mk/rust-release.mk",
        fragment,
        &RUST_RELEASE_PINS,
    ))
}

/// bitgauge's Dockerfile and release fragment pass; each way to move one copy fails with exactly
/// the problem that names it: another version on either side, a changed digest on either side,
/// swapped arms, a dropped aarch64 arm, a junk riscv64 arm, a digest the fragment holds for no
/// arm, and a stray sha256. Make's last assignment is the one read.
#[test]
fn release_pin_rules() {
    assert_eq!(
        release_check(BG_DOCKERFILE, BG_RELEASE_MK),
        Vec::<String>::new()
    );
    let std_amd64 = "bd9f35880de21dab0e387e84009c00001ae7836a4ba565cb16823f13f75f325f";
    let std_arm64 = "7e0ad2e67817f224b7a6331e48d6397303fc050edaccf81784ed786ffddc884a";
    let std_want = format!(
        "{{(\"amd64\", \"x86_64-unknown-linux-musl\", \"{std_amd64}\"), (\"arm64\", \
         \"aarch64-unknown-linux-musl\", \"{std_arm64}\")}}"
    );
    let std_arms = |found: &str| {
        format!(
            "Dockerfile: std_sha256 is chosen by the arms {found}, want exactly {std_want} from \
             .mk/rust-release.mk"
        )
    };
    let docker = |from: &str, to: &str| {
        assert!(BG_DOCKERFILE.contains(from), "{from:?} is gone");
        release_check(&BG_DOCKERFILE.replacen(from, to, 1), BG_RELEASE_MK)
    };
    let fragment = |from: &str, to: &str| {
        assert!(BG_RELEASE_MK.contains(from), "{from:?} is gone");
        release_check(BG_DOCKERFILE, &BG_RELEASE_MK.replacen(from, to, 1))
    };
    assert_eq!(
        docker("ARG ZIG_VERSION=0.17.0", "ARG ZIG_VERSION=0.16.0"),
        [
            "Dockerfile: writes ARG ZIG_VERSION as [\"0.16.0\"], want once as \
             .mk/rust-release.mk's 0.17.0"
        ]
    );
    assert_eq!(
        fragment(
            "RUST_RELEASE_RUST_STD_VERSION := 1.98.1",
            "RUST_RELEASE_RUST_STD_VERSION := 1.98.0"
        ),
        [
            "Dockerfile: writes ARG RUST_VERSION as [\"1.98.1\"], want once as \
             .mk/rust-release.mk's 1.98.0"
        ]
    );
    assert_eq!(
        release_check(
            BG_DOCKERFILE,
            &format!("{BG_RELEASE_MK}RUST_RELEASE_RUST_STD_VERSION := 1.98.0\n")
        )
        .len(),
        1,
        "make's last assignment wins"
    );
    let changed = format!("{}0", &std_amd64[..63]);
    assert_eq!(
        docker(std_amd64, &changed),
        [std_arms(&format!(
            "{{(\"amd64\", \"x86_64-unknown-linux-musl\", \"{changed}\"), (\"arm64\", \
                 \"aarch64-unknown-linux-musl\", \"{std_arm64}\")}}"
        )),]
    );
    assert_eq!(
        fragment(std_amd64, &changed).len(),
        1,
        "a changed digest in the fragment"
    );
}

/// The arms of bitgauge's Dockerfile against its release fragment: swapped, dropped and junk arms,
/// a digest the fragment holds for no arm, a stray sha256 and empty pins each fail.
#[test]
fn release_pin_arm_rules() {
    let std_arm64 = "7e0ad2e67817f224b7a6331e48d6397303fc050edaccf81784ed786ffddc884a";
    let changed = format!("{}0", &std_arm64[..63]);
    let docker = |from: &str, to: &str| {
        assert!(BG_DOCKERFILE.contains(from), "{from:?} is gone");
        release_check(&BG_DOCKERFILE.replacen(from, to, 1), BG_RELEASE_MK)
    };
    assert_eq!(
        docker(
            "amd64) triple=x86_64-unknown-linux-musl",
            "arm64) triple=x86_64-unknown-linux-musl"
        )
        .iter()
        .map(|problem| problem.starts_with("Dockerfile: std_sha256 is chosen by the arms "))
        .collect::<Vec<_>>(),
        [true],
        "swapped arms"
    );
    assert_eq!(
        docker(
            "\t\tarm64) triple=aarch64-unknown-linux-musl;",
            "\t\t*) triple=aarch64-unknown-linux-musl;"
        )
        .iter()
        .map(|problem| problem.starts_with("Dockerfile: std_sha256 is chosen by the arms "))
        .collect::<Vec<_>>(),
        [true],
        "a dropped aarch64 arm"
    );
    assert_eq!(
        docker(
            "\t\t*) echo \"no Rust target",
            "\t\triscv64) triple=riscv64gc-unknown-linux-musl; std_sha256=00 ;; \\\n\t\t*) echo \
             \"no Rust target"
        )
        .iter()
        .map(|problem| problem.starts_with("Dockerfile: std_sha256 is chosen by the arms "))
        .collect::<Vec<_>>(),
        [true],
        "a junk riscv64 arm"
    );
    assert_eq!(
        release_check(
            BG_DOCKERFILE,
            &format!(
                "{BG_RELEASE_MK}RUST_RELEASE_RUST_STD_SHA256_riscv64gc-unknown-linux-musl := \
                 {std_arm64}\n"
            )
        ),
        [
            ".mk/rust-release.mk: sets RUST_RELEASE_RUST_STD_SHA256_riscv64gc-unknown-linux-musl, \
             which no required arm names"
        ]
    );
    assert_eq!(
        release_check(&format!("{BG_DOCKERFILE}# {changed}\n"), BG_RELEASE_MK),
        [
            "Dockerfile: writes 8 sha256s: 6 release archives and 1 image digests account for \
             the rest, so one is not .mk/rust-release.mk's"
        ]
    );
    assert_eq!(
        strings(&release_pin_problems(
            "Dockerfile",
            BG_DOCKERFILE,
            ".mk/rust-release.mk",
            BG_RELEASE_MK,
            &ReleasePins {
                versions: &[],
                archives: &[],
            },
        )),
        ["the release pins name no version and no archive; the check would pass on nothing"]
    );
}

/// Another release, a missing, short, doubled or extra digest, and a platform list that leaves a
/// target out are each refused once.
#[test]
fn rust_std_rules() {
    let targets = toolchain_targets(
        "[toolchain]\nchannel = \"1.98.1\"\ntargets = [\"x86_64-unknown-linux-musl\", \
         \"aarch64-unknown-linux-musl\"]\n",
    );
    let amd64 = "bd9f35880de21dab0e387e84009c00001ae7836a4ba565cb16823f13f75f325f";
    let arm64 = "7e0ad2e67817f224b7a6331e48d6397303fc050edaccf81784ed786ffddc884a";
    let fragment = format!(
        "RUST_RELEASE_RUST_STD_VERSION := 1.98.1\n\
         RUST_RELEASE_RUST_STD_SHA256_x86_64-unknown-linux-musl := {amd64}\n\
         RUST_RELEASE_RUST_STD_SHA256_aarch64-unknown-linux-musl := {arm64}\n\
         RUST_RELEASE_PLATFORMS := amd64:x86_64-unknown-linux-musl \
         arm64:aarch64-unknown-linux-musl\n"
    );
    let check = |fragment: &str, release: &str, targets: &[String]| {
        rust_std_problems(".mk/rust-release.mk", fragment, release, targets)
    };
    assert_eq!(check(&fragment, "1.98.1", &targets), []);
    assert!(!check(&fragment, "1.98.1", &[]).is_empty());
    let arm64_line =
        format!("RUST_RELEASE_RUST_STD_SHA256_aarch64-unknown-linux-musl := {arm64}\n");
    for (name, sample, release) in [
        ("another release", fragment.clone(), "1.98.0"),
        (
            "no rust-std version",
            fragment.replace("RUST_RELEASE_RUST_STD_VERSION := 1.98.1\n", ""),
            "1.98.1",
        ),
        (
            "a missing digest",
            fragment.replace(&arm64_line, ""),
            "1.98.1",
        ),
        (
            "a short digest",
            fragment.replace(arm64, &arm64[1..]),
            "1.98.1",
        ),
        (
            "a doubled digest",
            format!("{fragment}{arm64_line}"),
            "1.98.1",
        ),
        (
            "a digest of another target",
            format!(
                "{fragment}RUST_RELEASE_RUST_STD_SHA256_riscv64gc-unknown-linux-musl := {arm64}\n"
            ),
            "1.98.1",
        ),
        (
            "a platform left out",
            fragment.replace(" arm64:aarch64-unknown-linux-musl", ""),
            "1.98.1",
        ),
    ] {
        let problems = check(&sample, release, &targets);
        assert_eq!(problems.len(), 1, "{name}: {problems:#?}");
    }
}

/// A moved hook, a moved action and a missing pin are each a problem.
#[test]
fn shellcheck_rules() {
    let hooks = "  - repo: https://github.com/shellcheck-py/shellcheck-py\n    rev: v0.11.0.1\n";
    let action = "        SHELLCHECK_VERSION: v0.11.0\n";
    let check = |hooks: &str, action: &str| {
        shellcheck_problems(".pre-commit-config.yaml", hooks, "action.yaml", action)
    };
    assert_eq!(check(hooks, action), []);
    for (name, hooks, action) in [
        (
            "a moved action",
            hooks.to_owned(),
            action.replace("v0.11.0", "v0.10.0"),
        ),
        (
            "a moved hook",
            hooks.replace("rev: v0.11.0.1", "rev: v0.12.0.1"),
            action.to_owned(),
        ),
        ("a missing pin", hooks.to_owned(), String::new()),
    ] {
        assert!(!check(&hooks, &action).is_empty(), "{name} was accepted");
    }
}

/// A floating version, a second pin, no setup-trivy step, a step with its own version, and two
/// workflows that disagree are each refused once.
#[test]
fn trivy_rules() {
    let good = "env:\n  TRIVY_VERSION: v0.74.0\njobs:\n  release:\n    steps:\n      \
                - uses: aquasecurity/setup-trivy@0123456789abcdef0123456789abcdef01234567\n        \
                with:\n          version: ${{ env.TRIVY_VERSION }}\n";
    let one = |content: &str| trivy_problems(&[("release.yaml".to_owned(), content.to_owned())]);
    assert_eq!(one(good), []);
    for (name, sample) in [
        (
            "a floating version",
            good.replace("TRIVY_VERSION: v0.74.0", "TRIVY_VERSION: latest"),
        ),
        (
            "a second pin",
            good.replace("jobs:", "  OTHER:\n    TRIVY_VERSION: v0.73.0\njobs:"),
        ),
        (
            "no setup-trivy step",
            good.replace("aquasecurity/setup-trivy", "some/other-action"),
        ),
        (
            "a step with its own version",
            good.replace("${{ env.TRIVY_VERSION }}", "v0.73.0"),
        ),
    ] {
        assert_eq!(one(&sample).len(), 1, "{name} was not refused once");
    }
    let scan = good.replace("v0.74.0", "v0.73.0");
    assert_eq!(
        strings(&trivy_problems(&[
            ("release.yaml".to_owned(), good.to_owned()),
            ("scheduled-scan.yaml".to_owned(), scan),
        ])),
        ["Trivy is not one value: v0.73.0 in scheduled-scan.yaml; v0.74.0 in release.yaml"]
    );
    assert_eq!(
        strings(&trivy_problems(&[])),
        ["no scanning workflow is given; the Trivy check would pass on nothing"]
    );
    assert_eq!(
        strings(&one(&good.replace("  TRIVY_VERSION: v0.74.0\n", ""))),
        ["release.yaml: writes TRIVY_VERSION 0 times, want once"]
    );
    assert_eq!(
        one(&good.replace("v0.74.0", "v0.74.0 # pinned with the scan")),
        [],
        "a trailing comment is read past"
    );
}

/// How make values are read: each flavour, `export` and `override` read past, the last that
/// sets the value winning; and a release pin read through them.
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
    for line in [
        "override RUST_RELEASE_RUST_STD_VERSION := 1.98.0",
        "RUST_RELEASE_RUST_STD_VERSION += 1",
    ] {
        assert_eq!(
            release_check(BG_DOCKERFILE, &format!("{BG_RELEASE_MK}{line}\n")).len(),
            1,
            "{line}"
        );
    }
}
