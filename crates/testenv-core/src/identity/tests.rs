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

//! The identity rule on each suite's recorded answers, the excerpt in both quotings, and the ELF
//! check on synthetic headers and this test binary.
//!
//! Extracted from swarm-device-access/testenv/src/identity.rs at 8b4ab3a3e417 (its tests, and its
//! `testdata/go_dash_help.*`) and bitgauge/testenv/src/identity/tests.rs at 600fc3905200 (its
//! recorded answers, `testdata/*_dash_help.*`).

use super::{Answer, IDENTITY_FLAG, Quoting, Text, excerpt, is_static_elf};

/// swarm-device-access's Go daemon's answer to `-help` (go1.26.10, `dist/swarm-device-access
/// -help`): exit 0, nothing on stdout.
const SDA_GO_STDOUT: &str = include_str!("../../tests/testdata/identity/sda_go_dash_help.stdout");
const SDA_GO_STDERR: &str = include_str!("../../tests/testdata/identity/sda_go_dash_help.stderr");
/// swarm-device-access's Rust daemon's refusal (`src/cli.rs`, `refuse_single_dash`).
const SDA_RUST_REFUSAL: &str =
    "error: single-dash long flags are not supported: '-help'; use '--help'\n";

/// bitgauge's Go binary's answer (`ghcr.io/leinardi/bitgauge:966a616-dirty`): exit 0.
const BG_GO_STDOUT: &str =
    include_str!("../../tests/testdata/identity/bitgauge_go_dash_help.stdout");
const BG_GO_STDERR: &str =
    include_str!("../../tests/testdata/identity/bitgauge_go_dash_help.stderr");
/// bitgauge's Rust binary's answer (`bitgauge:rust-candidate` at e5fcc5f): exit 2, stdout empty.
const BG_RUST_STDERR: &str =
    include_str!("../../tests/testdata/identity/bitgauge_rust_dash_help.stderr");

const SDA_RUST: Answer = Answer {
    exit_code: 2,
    stdout: Text::Empty,
    stderr: Text::Exact(SDA_RUST_REFUSAL),
};
const SDA_GO: Answer = Answer {
    exit_code: 0,
    stdout: Text::Empty,
    stderr: Text::Prefix("Usage: swarm-device-access [flags]\n"),
};
const BG_RUST: Answer = Answer {
    exit_code: 2,
    stdout: Text::Empty,
    stderr: Text::Exact(
        "bitgauge: -help is not an option; write it as --help\nUsage: bitgauge [OPTIONS]\n       \
         bitgauge <COMMAND>\n",
    ),
};
const BG_GO: Answer = Answer {
    exit_code: 0,
    stdout: Text::Exact(
        "usage: bitgauge [serve|healthcheck] [--config /etc/bitgauge/config.yaml]\n",
    ),
    stderr: Text::Prefix("Usage of bitgauge serve:\n"),
};

#[test]
fn the_flag_is_go_s_help() {
    assert_eq!(IDENTITY_FLAG, "-help");
}

/// Each build's recorded answer is its own and not the other's, in both suites.
#[test]
fn each_answer_identifies_its_build() {
    assert!(SDA_RUST.matches(Some(2), "", SDA_RUST_REFUSAL));
    assert!(SDA_GO.matches(Some(0), SDA_GO_STDOUT, SDA_GO_STDERR));
    assert!(!SDA_RUST.matches(Some(0), SDA_GO_STDOUT, SDA_GO_STDERR));
    assert!(!SDA_GO.matches(Some(2), "", SDA_RUST_REFUSAL));

    assert!(BG_RUST.matches(Some(2), "", BG_RUST_STDERR));
    assert!(BG_GO.matches(Some(0), BG_GO_STDOUT, BG_GO_STDERR));
    assert!(!BG_RUST.matches(Some(0), BG_GO_STDOUT, BG_GO_STDERR));
    assert!(!BG_GO.matches(Some(2), "", BG_RUST_STDERR));
}

/// Every part of the answer counts: the code, the text and which stream it is on.
#[test]
fn a_partial_match_is_refused() {
    let mut accepted = Vec::new();
    for (name, answer, exit_code, stdout, stderr) in [
        ("exit 0", SDA_RUST, Some(0), "", SDA_RUST_REFUSAL),
        ("exit 1", SDA_RUST, Some(1), "", SDA_RUST_REFUSAL),
        ("killed", SDA_RUST, None, "", SDA_RUST_REFUSAL),
        (
            "Go's own usage error",
            SDA_RUST,
            Some(2),
            "",
            "flag provided but not defined: -x\n",
        ),
        (
            "no newline",
            SDA_RUST,
            Some(2),
            "",
            SDA_RUST_REFUSAL.trim_end(),
        ),
        (
            "output on stdout",
            SDA_RUST,
            Some(2),
            "Usage:\n",
            SDA_RUST_REFUSAL,
        ),
        ("nothing", SDA_RUST, Some(2), "", ""),
        (
            "the refusal twice",
            SDA_RUST,
            Some(2),
            "",
            &SDA_RUST_REFUSAL.repeat(2),
        ),
        ("Go exit 2", SDA_GO, Some(2), SDA_GO_STDOUT, SDA_GO_STDERR),
        ("Go usage on stdout", SDA_GO, Some(0), SDA_GO_STDERR, ""),
        ("Go output on both", SDA_GO, Some(0), "x\n", SDA_GO_STDERR),
        (
            "another usage",
            SDA_GO,
            Some(0),
            "",
            "Usage of swarm-device-access:\n",
        ),
        (
            "bitgauge Go usage on stderr",
            BG_GO,
            Some(0),
            "",
            BG_GO_STDOUT,
        ),
        (
            "bitgauge Go without the defaults",
            BG_GO,
            Some(0),
            BG_GO_STDOUT,
            "",
        ),
        (
            "bitgauge Rust refusal alone",
            BG_RUST,
            Some(2),
            "",
            BG_RUST_STDERR.lines().next().unwrap(),
        ),
    ] {
        if answer.matches(exit_code, stdout, stderr) {
            accepted.push(name);
        }
    }
    assert!(accepted.is_empty(), "accepted: {accepted:?}");
}

/// The excerpt the suites' messages quote, as each words it.
#[test]
fn the_excerpt_quotes_as_each_suite() {
    // swarm-device-access: Go's quoting of a long answer's first line.
    assert_eq!(
        excerpt(SDA_GO_STDERR, Quoting::Go),
        r#""Usage: swarm-device-access [flags]" (first line of 1285 bytes)"#
    );
    assert_eq!(excerpt(SDA_GO_STDOUT, Quoting::Go), r#""""#);
    // bitgauge: Rust's quoting, the whole of a one-line answer.
    assert_eq!(
        excerpt(BG_GO_STDOUT, Quoting::Debug),
        r#""usage: bitgauge [serve|healthcheck] [--config /etc/bitgauge/config.yaml]\n""#
    );
    assert_eq!(
        excerpt(BG_GO_STDERR, Quoting::Debug),
        r#""Usage of bitgauge serve:" (first line of 121 bytes)"#
    );
    // The two quotings part on a control character.
    assert_eq!(excerpt("a\u{1}", Quoting::Go), r#""a\x01""#);
    assert_eq!(excerpt("a\u{1}", Quoting::Debug), r#""a\u{1}""#);
}

/// A minimal ELF64 header with `kinds` as its program header types.
fn elf(kinds: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
    bytes[0x20..0x28].copy_from_slice(&64_u64.to_le_bytes());
    bytes[0x36..0x38].copy_from_slice(&56_u16.to_le_bytes());
    let count = u16::try_from(kinds.len()).expect("too many headers");
    bytes[0x38..0x3a].copy_from_slice(&count.to_le_bytes());
    for kind in kinds {
        let mut header = vec![0_u8; 56];
        header[..4].copy_from_slice(&kind.to_le_bytes());
        bytes.extend(header);
    }
    bytes
}

#[test]
fn a_loader_makes_a_binary_dynamic() {
    assert!(is_static_elf(&elf(&[6, 1, 1])).expect("parse"));
    assert!(!is_static_elf(&elf(&[6, 3, 1])).expect("parse"));
    assert!(is_static_elf(&elf(&[])).expect("parse"));
}

#[test]
fn what_is_not_an_elf64_file_is_refused() {
    assert_eq!(
        is_static_elf(b"#!/bin/sh\n").unwrap_err().to_string(),
        "not a 64-bit little-endian ELF file"
    );
    assert!(is_static_elf(&[]).is_err());
    let mut thirty_two_bit = elf(&[1]);
    thirty_two_bit[4] = 1;
    assert!(is_static_elf(&thirty_two_bit).is_err());
    let mut truncated = elf(&[1, 1]);
    truncated.truncate(64 + 56);
    assert_eq!(
        is_static_elf(&truncated).unwrap_err().to_string(),
        "program header 1 lies outside the file"
    );
}

/// The real-file control: this test binary, linked against glibc, names its loader.
#[cfg(target_env = "gnu")]
#[test]
fn this_glibc_test_binary_is_dynamic() {
    let path = std::env::current_exe().expect("current exe");
    let bytes = std::fs::read(&path).expect("read the test binary");
    assert!(!is_static_elf(&bytes).expect("parse"), "{}", path.display());
}
