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

//! The harness, driving rust-common's own suppression ratchet: a
//! verdict the case expects is no problem, a wrong one is, and so is a refusal with other text.

use std::path::Path;

use super::{RatchetCase, SuppressionHarness};
use crate::Repo;

/// rust-common's root: this crate sits two levels below it.
fn rust_common() -> Repo {
    Repo::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

const RATCHET: SuppressionHarness<'static> = SuppressionHarness {
    script: "scripts/check-suppressions.sh",
    baseline: ".suppressions-baseline",
};

const ONE: &str = "#[expect(clippy::too_many_lines, reason = \"scratch\")]\nfn run() {}\n";
const TWO: &str = "#[expect(clippy::too_many_lines, reason = \"scratch\")]\nfn run() {}\n\
                   #[expect(clippy::too_many_lines, reason = \"scratch too\")]\nfn walk() {}\n";

/// The ratchet accepts a tree that matches its baseline and refuses one above it, naming the
/// count; a case expecting the opposite, or another refusal, is reported.
#[test]
fn the_ratchet_harness_reports_wrong_verdicts_only() {
    let repo = rust_common();
    let cases: [RatchetCase; 2] = [
        (
            "matching",
            |scratch| scratch.commit_initial("x.rs", ONE, "expect/clippy::too_many_lines\t1"),
            None,
        ),
        (
            "above the baseline",
            |scratch| scratch.commit_initial("x.rs", TWO, "expect/clippy::too_many_lines\t1"),
            Some("baseline allows 1"),
        ),
    ];
    let problems = RATCHET.run_cases(&repo, &cases);
    assert!(problems.is_empty(), "{}", crate::report(&problems));

    let wrong: [RatchetCase; 3] = [
        (
            "wrongly expected to pass",
            |scratch| scratch.commit_initial("x.rs", TWO, "expect/clippy::too_many_lines\t1"),
            None,
        ),
        (
            "wrongly expected to fail",
            |scratch| scratch.commit_initial("x.rs", ONE, "expect/clippy::too_many_lines\t1"),
            Some("baseline allows 1"),
        ),
        (
            "refused for another reason",
            |scratch| scratch.commit_initial("x.rs", TWO, "expect/clippy::too_many_lines\t1"),
            Some("no such complaint"),
        ),
    ];
    let problems = RATCHET.run_cases(&repo, &wrong);
    let names: Vec<&str> = problems
        .iter()
        .map(|problem| problem.location.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "wrongly expected to pass",
            "wrongly expected to fail",
            "refused for another reason"
        ]
    );
}

/// The baseline helpers write what the ratchet reads: `--update` over a staged file writes its
/// count into the scratch baseline.
#[test]
fn the_scratch_baseline_is_what_the_ratchet_writes() {
    let scratch = RATCHET.scratch(&rust_common());
    scratch.commit_empty_baseline();
    scratch.write("x.rs", TWO);
    scratch.git(&["add", "x.rs"]);
    let (accepted, output) = scratch.ratchet(&["--update"]);
    assert!(accepted, "{output}");
    assert!(
        scratch
            .read(".suppressions-baseline")
            .contains("expect/clippy::too_many_lines\t2\n"),
        "{}",
        scratch.read(".suppressions-baseline")
    );
    assert!(scratch.repo().path().join("x.rs").is_file());
}

/// A blank refusal text matches any refusal, so it is refused before any case runs.
#[test]
fn a_blank_refusal_text_is_refused() {
    let cases: [RatchetCase; 2] = [
        ("empty", |scratch| scratch.commit_empty_baseline(), Some("")),
        (
            "blank",
            |scratch| scratch.commit_empty_baseline(),
            Some("  "),
        ),
    ];
    let problems = RATCHET.run_cases(&rust_common(), &cases);
    let names: Vec<&str> = problems
        .iter()
        .map(|problem| problem.location.as_str())
        .collect();
    assert_eq!(names, ["empty", "blank"]);
}
