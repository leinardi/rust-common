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

//! The harnesses, driving rust-common's own licence-header checker and suppression ratchet: a
//! verdict the case expects is no problem, a wrong one is, and so is a refusal with other text.

use std::path::Path;

use super::{HeaderCase, HeaderHarness, RatchetCase, SuppressionHarness};
use crate::Repo;

/// rust-common's root: this crate sits two levels below it.
fn rust_common() -> Repo {
    Repo::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

const HEADER: &str = "/*
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

fn x() {}
";

const MAKEFILE: &str = "#
# Copyright (c) 2026 Roberto Leinardi
#
# Licensed under either of
#
#  * Apache License, Version 2.0 (LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0)
#  * MIT license (LICENSE-MIT or https://opensource.org/licenses/MIT)
#
# at your option.
#
# SPDX-License-Identifier: MIT OR Apache-2.0
#
MK_COMMON_FILES := help.mk rust.mk
";

const HEADERS: HeaderHarness<'static> = HeaderHarness {
    script: "scripts/check-license-headers.sh",
    base_files: &[("Makefile", MAKEFILE)],
    untracked_prefix: "untracked/",
};

/// The checker accepts a header and an untracked file, rejects a missing header, and a case
/// whose expectation is wrong is reported, naming the case.
#[test]
fn the_header_harness_reports_wrong_verdicts_only() {
    let repo = rust_common();
    let cases = [
        HeaderCase::new("with the header", &[("src/x.rs", HEADER)], true),
        HeaderCase::new("without a header", &[("src/x.rs", "fn x() {}\n")], false),
        HeaderCase::new("untracked", &[("untracked/x.rs", "fn x() {}\n")], true),
        HeaderCase::new(
            "an upstream make fragment",
            &[(".mk/rust.mk", "x:\n")],
            true,
        ),
        HeaderCase::new(
            "a Makefile without one",
            &[("Makefile", "MK_COMMON_FILES := help.mk\n")],
            false,
        ),
    ];
    let problems = HEADERS.run_cases(&repo, &cases);
    assert!(problems.is_empty(), "{}", crate::report(&problems));

    let wrong = [
        HeaderCase::new(
            "wrongly expected to pass",
            &[("src/x.rs", "fn x() {}\n")],
            true,
        ),
        HeaderCase::new("wrongly expected to fail", &[("src/x.rs", HEADER)], false),
    ];
    let problems = HEADERS.run_cases(&repo, &wrong);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert_eq!(problems[0].location, "wrongly expected to pass");
    assert!(
        problems[0].message.starts_with("rejected, want accepted:"),
        "{problems:?}"
    );
    assert_eq!(problems[1].location, "wrongly expected to fail");
    assert!(
        problems[1].message.starts_with("accepted, want rejected:"),
        "{problems:?}"
    );
}

/// A checker that cannot be read is one problem, not a pass on no case.
#[test]
fn a_missing_checker_is_a_problem() {
    let harness = HeaderHarness {
        script: "scripts/no-such-checker.sh",
        ..HEADERS
    };
    let problems = harness.run_cases(&rust_common(), &[]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].location, "scripts/no-such-checker.sh");
}

/// The repository's own run: rust-common passes its checker, and a scratch repository whose file
/// lacks the header does not.
#[test]
fn the_repository_run_reports_a_rejection() {
    assert_eq!(HEADERS.check_repository(&rust_common()), []);

    let scratch = super::ScratchRepo::new();
    scratch.write(HEADERS.script, &rust_common().read(HEADERS.script));
    scratch.write("Makefile", MAKEFILE);
    scratch.write("src/x.rs", "fn x() {}\n");
    std::fs::set_permissions(
        scratch.path().join(HEADERS.script),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    scratch.git(&["add", "-A"]);
    let problems = HEADERS.check_repository(&Repo::scratch(scratch.path()));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].message.contains("src/x.rs"), "{problems:?}");
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
