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

//! The flags' negative controls: each flag, written as a gate would carry it, is found; a clippy
//! gate that denies warnings and a hook's or step's label are not; an exception covers only its
//! own fragment in its own file; and an unused exception is reported.
//!
//! Extracted from swarm-device-access/meta/tests/vacuous_flags.rs at 8b4ab3a3e417
//! (`vacuous_flags_are_recognized`, `vacuous_hit_numbering`) and
//! `bitgauge/meta/tests/vacuous_flags.rs` at 600fc3905200 (the JavaScript samples).

use super::{AllowedFragment, Hit, VacuousFlags};
use crate::strings;

const RELEASE: &str = ".github/workflows/release.yaml";

const ALLOWED: [AllowedFragment<'static>; 2] = [
    AllowedFragment {
        file: RELEASE,
        fragment: r#"{ grep -E "$RELEASE_TAG_REGEX" || true; }"#,
        reason: "grep exits 1 when no tag matches",
    },
    AllowedFragment {
        file: RELEASE,
        fragment: r#"{ git tag --merged "HEAD^" 2>/dev/null || true; }"#,
        reason: "HEAD^ does not exist on a root commit",
    },
];

fn sample_hits(flags: &VacuousFlags, sample: &str) -> Vec<Hit> {
    flags.hits("Makefile", sample, &ALLOWED, &mut [false; 2])
}

const CORE_SAMPLES: [&str; 13] = [
    "\tgo vet ./... || true",
    "\tgo vet ./... ||:",
    "\tgo vet ./... || exit 0",
    "\t-go vet ./...",
    "\t@-go vet ./...",
    ".IGNORE: go-test",
    "\t$(MAKE) -i go-test",
    "\t$(MAKE) --ignore-errors go-test",
    "        continue-on-error: true",
    "    if: false",
    "    if: ${{ false }}",
    "    if: 'false'",
    "    if: \"${{ false }}\"",
];

const RUST_SAMPLES: [&str; 10] = [
    "\tcargo test --workspace --locked --no-run",
    "        run: cargo clippy --workspace -- --cap-lints warn",
    "\tcargo clippy --workspace -- -A warnings -D warnings",
    "\tcargo clippy --workspace -- --allow=warnings -D warnings",
    "        entry: cargo test --workspace --locked -- --skip slow",
    "\tcargo test --workspace --exclude meta --locked",
    "\t$(CARGO) test --workspace --locked --no-run",
    "        run: cargo clippy --workspace --all-targets --locked",
    "\t$(CARGO) clippy --workspace --all-targets --all-features --locked",
    "        entry: cargo clippy --workspace --all-targets --locked",
];

const JAVASCRIPT_SAMPLES: [&str; 2] = [
    r#""test": "vitest run --passWithNoTests""#,
    r#""lint": "eslint src --no-error-on-unmatched-pattern""#,
];

#[test]
fn each_set_finds_its_flags() {
    let all = VacuousFlags::core().with_rust().with_javascript();
    for sample in CORE_SAMPLES
        .iter()
        .chain(&RUST_SAMPLES)
        .chain(&JAVASCRIPT_SAMPLES)
    {
        assert!(
            !sample_hits(&all, sample).is_empty(),
            "no vacuous flag found in {sample:?}"
        );
    }
    let core = VacuousFlags::core();
    for sample in CORE_SAMPLES {
        assert!(!sample_hits(&core, sample).is_empty(), "{sample:?}");
    }
    // The core set alone does not hold a project to the Rust or JavaScript rules.
    for sample in RUST_SAMPLES.iter().chain(&JAVASCRIPT_SAMPLES) {
        assert!(sample_hits(&core, sample).is_empty(), "{sample:?}");
    }
    assert_eq!(VacuousFlags::core().with_rust().flags().len(), 11);
}

#[test]
fn a_gate_that_denies_warnings_and_a_label_pass() {
    let flags = VacuousFlags::core().with_rust();
    for sample in [
        "\t$(CARGO) clippy --workspace --all-targets --all-features --locked -- -D warnings",
        "        entry: cargo clippy --workspace --all-targets --locked -- --deny=warnings",
        "        name: cargo clippy",
        "      - name: Lint with clippy",
        "        entry: cargo test --locked -p meta --test docs",
    ] {
        let hits = sample_hits(&flags, sample);
        assert!(hits.is_empty(), "{sample:?} was flagged: {hits:?}");
    }
}

#[test]
fn an_exception_covers_its_own_fragment_in_its_own_file() {
    let flags = VacuousFlags::core();
    let allowed = r#"            { grep -E "$RELEASE_TAG_REGEX" || true; } | sort -V | tail -n1"#;
    let mut used = [false; 2];
    assert_eq!(flags.hits(RELEASE, allowed, &ALLOWED, &mut used), []);
    assert_eq!(used, [true, false]);
    let extended = format!("{allowed} || true");
    for (file, sample) in [
        (".github/workflows/ci.yaml", allowed),
        (RELEASE, extended.as_str()),
        (
            ".github/workflows/x.yaml",
            r#"  { git tag --merged "HEAD^" 2>/dev/null || true; }"#,
        ),
    ] {
        assert!(
            !flags
                .hits(file, sample, &ALLOWED, &mut [false; 2])
                .is_empty(),
            "{file}: {sample:?} passed through the allowlist"
        );
    }
}

/// A hit names its file and line, and an exception that matches nothing is reported.
#[test]
fn problems_name_the_line_and_the_unused_exceptions() {
    let flags = VacuousFlags::core();
    let files = [
        (
            "Makefile".to_owned(),
            "all:\n\tgo vet ./...\n\tgo test ./... || true\n".to_owned(),
        ),
        (
            RELEASE.to_owned(),
            r#"run: { grep -E "$RELEASE_TAG_REGEX" || true; }"#.to_owned(),
        ),
    ];
    assert_eq!(
        strings(&flags.problems(&files, &ALLOWED)),
        [
            "Makefile:3: || true: go test ./... || true",
            r#".github/workflows/release.yaml: the exception "{ git tag --merged \"HEAD^\" 2>/dev/null || true; }" (HEAD^ does not exist on a root commit) matches nothing; remove it"#,
        ]
    );
}

/// No file is a problem, not a pass on nothing.
#[test]
fn no_file_is_a_problem() {
    assert_eq!(
        strings(&VacuousFlags::core().problems(&[], &[])),
        ["no gate file to scan; the check would pass on nothing"]
    );
}
