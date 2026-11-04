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

//! Drives scripts/check-suppressions.sh, the hook that keeps every in-file linter suppression
//! counted against a reviewed baseline, through `repo_meta::scratch::SuppressionHarness`.
//!
//! Each case builds a scratch git repository and runs the script inside it, rather than against
//! this repository: what is under test is the rule, not today's counts, and a case has to be able
//! to add a suppression without that being a suppression anybody has to review.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/suppressions.rs at c9e79afa8810, without the
//! droast families and the bootstrap-script exclusion, which this repository's ratchet does not
//! have (swarm-device-access/scripts/check-suppressions.sh at 8b4ab3a3e417, as adapted here).

use repo_meta::report;
use repo_meta::scratch::{RatchetCase, RatchetScratch, SuppressionHarness};

const HARNESS: SuppressionHarness<'static> = SuppressionHarness {
    script: "scripts/check-suppressions.sh",
    baseline: ".suppressions-baseline",
};

/// The name the script looks for, relative to the repository it is run in.
const BASELINE_FILE: &str = ".suppressions-baseline";

/// The scratch Rust file every case edits.
const SCRATCH: &str = "scratch.rs";

/// One `#[expect(clippy::too_many_lines)]`.
const RUST_WITH_ONE: &str = r#"#[expect(clippy::too_many_lines, reason = "scratch fixture")]
fn run() {}
"#;

/// The same single attribute on a different line: the count is unchanged, which is the case the
/// per-rule counting alone cannot see.
const RUST_WITH_ONE_MOVED: &str = r#"fn first() {}

#[expect(clippy::too_many_lines, reason = "scratch fixture, moved")]
fn run() {}
"#;

/// Two attributes for the same lint.
const RUST_WITH_TWO: &str = r#"#[expect(clippy::too_many_lines, reason = "scratch fixture")]
fn run() {}

#[expect(clippy::too_many_lines, reason = "scratch fixture too")]
fn walk() {}
"#;

/// An attribute after a string holding a comment opener on the same line. A detector that stops
/// at the first "//" on the line would miss it.
const RUST_AFTER_URL: &str = r#"const URL: &str = "https://example.com/x"; #[expect(clippy::too_many_lines, reason = "behind a URL")]
fn run() {}
"#;

/// A sentence that mentions an attribute. It is not a suppression, and counting it as one would
/// make every comment about the ratchet fail the ratchet.
const RUST_PROSE_MENTION: &str =
    "// run is written so that it needs no #[expect(clippy::too_many_lines)] at all.
fn run() {}
";

/// An attribute mentioned inside a block comment, which is not code either.
const RUST_BLOCK_COMMENT: &str = "/*
 * run needs no #[expect(clippy::too_many_lines)], and this line is not one.
 */
fn run() {}
";

/// Attribute text held in a raw string spanning lines. None of it is a suppression.
const RUST_RAW_STRING_FIXTURE: &str = r##"const FIXTURE: &str = r#"
#[expect(clippy::too_many_lines, reason = "inside a raw string")]
fn run() {}
"#;
"##;

/// One more line added inside that raw string, a line the staged-diff rule must not read as an
/// attribute.
const RUST_RAW_STRING_FIXTURE_EXTENDED: &str = r##"const FIXTURE: &str = r#"
#[expect(clippy::cast_possible_truncation, reason = "inside a raw string too")]
#[expect(clippy::too_many_lines, reason = "inside a raw string")]
fn run() {}
"#;
"##;

/// A real attribute added after the raw string closes, which the same rule must still catch.
const RUST_RAW_STRING_FIXTURE_AND_ATTRIBUTE: &str = r##"const FIXTURE: &str = r#"
#[expect(clippy::too_many_lines, reason = "inside a raw string")]
fn run() {}
"#;

#[expect(clippy::too_many_lines, reason = "in code this time")]
fn real() {}
"##;

/// A shellcheck directive.
const SHELL_WITH_DISABLE: &str = "#!/usr/bin/env bash
# shellcheck disable=SC2086
echo $NAMES
";

/// Every Rust lint-attribute form the ratchet counts: an inner and an outer expect, an allow, and
/// a multi-line expect naming two clippy lints the way rustfmt wraps a long attribute. The reason
/// item is not a lint, commas and parentheses inside it included.
const RUST_WITH_ATTRIBUTES: &str = r#"#![expect(clippy::pedantic, reason = "scratch fixture, (inner)")]

#[expect(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "scratch fixture"
)]
fn long() {}

#[allow(dead_code)]
fn unused() {}

#![allow(missing_docs)]
"#;

/// Lint attributes only as data: in a raw string spanning lines, in a string, in a line comment and
/// in a nested block comment, around character literals holding a quote. None of it is a
/// suppression.
const RUST_WITH_ATTRIBUTE_TEXT: &str = r##"const FIXTURE: &str = r#"
#[allow(clippy::unwrap_used)]
"#;
const TEXT: &str = "#[expect(dead_code)]";
// #[allow(dead_code)]
/* #[allow(dead_code)] /* nested */ #[allow(dead_code)] */
fn quote<'a>(text: &'a str) -> char { let _quote = '"'; '#' }
"##;

/// A multi-line expect naming one lint.
const RUST_WITH_MULTI_LINE_EXPECT: &str = r#"#[expect(
    clippy::too_many_lines,
    reason = "scratch fixture"
)]
fn long() {}
"#;

/// A second lint added on its own line inside the same attribute: the opening line is untouched,
/// so only the line naming the lint shows the addition.
const RUST_WITH_MULTI_LINE_EXPECT_EXTENDED: &str = r#"#[expect(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "scratch fixture"
)]
fn long() {}
"#;

fn new_scratch() -> RatchetScratch {
    HARNESS.scratch(&meta::repo())
}

fn run_cases(cases: &[RatchetCase]) {
    let problems = HARNESS.run_cases(&meta::repo(), cases);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The staged-diff and count rules: each state the ratchet must accept or refuse.
#[test]
fn check_suppressions() {
    run_cases(&[
        (
            "a tree that matches its baseline passes",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_WITH_ONE, "expect/clippy::too_many_lines\t1");
            },
            None,
        ),
        (
            "an added suppression with an untouched baseline is refused",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_WITH_ONE, "expect/clippy::too_many_lines\t1");
                scratch.write(SCRATCH, RUST_WITH_TWO);
                scratch.git(&["add", SCRATCH]);
            },
            Some("without touching .suppressions-baseline"),
        ),
        (
            "the same addition passes once the baseline is staged with it",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_WITH_ONE, "expect/clippy::too_many_lines\t1");
                scratch.write(SCRATCH, RUST_WITH_TWO);
                scratch.write_baseline("expect/clippy::too_many_lines\t2");
                scratch.git(&["add", SCRATCH, BASELINE_FILE]);
            },
            None,
        ),
        (
            "a suppression swapped one for one is refused even though the count holds",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_WITH_ONE, "expect/clippy::too_many_lines\t1");
                scratch.write(SCRATCH, RUST_WITH_ONE_MOVED);
                scratch.git(&["add", SCRATCH]);
            },
            Some("without touching .suppressions-baseline"),
        ),
        (
            "a count above the baseline is refused with nothing staged",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_WITH_TWO, "expect/clippy::too_many_lines\t1");
            },
            Some("baseline allows 1"),
        ),
        (
            "a count below the baseline is refused too",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_WITH_ONE, "expect/clippy::too_many_lines\t3");
            },
            Some("baseline still claims 3"),
        ),
        (
            "a rule the baseline has never heard of is refused",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_WITH_ONE, "expect/clippy::float_cmp\t1");
            },
            Some("does not list this rule"),
        ),
    ]);
}

/// The detector's boundaries and the missing baseline: what counts as a suppression, and that no
/// baseline is never read as no suppressions.
#[test]
fn check_suppressions_detector_and_missing_baseline() {
    run_cases(&[
        (
            "an attribute behind a string containing a comment opener still counts",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_AFTER_URL, "expect/clippy::float_cmp\t1");
            },
            Some("expect/clippy::too_many_lines"),
        ),
        (
            "an attribute mentioned in prose is not a suppression",
            |scratch| scratch.commit_initial(SCRATCH, RUST_PROSE_MENTION, ""),
            None,
        ),
        (
            "an attribute inside a block comment is not a suppression either",
            |scratch| scratch.commit_initial(SCRATCH, RUST_BLOCK_COMMENT, ""),
            None,
        ),
        (
            "a deleted baseline is refused rather than treated as no suppressions",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_WITH_ONE, "expect/clippy::too_many_lines\t1");
                scratch.git(&["rm", "-q", BASELINE_FILE]);
            },
            Some("is missing"),
        ),
        (
            "a deleted baseline is refused in a tree with no suppressions at all",
            |scratch| {
                scratch.commit_initial(SCRATCH, RUST_PROSE_MENTION, "");
                scratch.git(&["rm", "-q", BASELINE_FILE]);
            },
            Some("is missing"),
        ),
    ]);
}

/// --update writes a baseline the checker then accepts: the script's own instruction for a
/// refusal is to run it, so a broken --update would leave no way out.
#[test]
fn update_writes_an_accepted_baseline() {
    let scratch = new_scratch();
    scratch.commit_initial(SCRATCH, RUST_WITH_TWO, "expect/clippy::too_many_lines\t1");
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        !accepted,
        "the ratchet accepted a count above its baseline:\n{output}"
    );

    let (updated, output) = scratch.ratchet(&["--update"]);
    assert!(updated, "--update failed:\n{output}");
    let written = scratch.read(BASELINE_FILE);
    assert!(
        written.contains("expect/clippy::too_many_lines\t2"),
        "the rewritten baseline does not record the two attributes:\n{written}"
    );

    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        accepted,
        "the ratchet still refuses the tree after --update:\n{output}"
    );
}

/// The detector across every family at once: the baseline is keyed per rule, so a family that
/// resolves to the wrong key, or to no key, takes its suppressions out of the ratchet without
/// anything else noticing.
#[test]
fn counts_every_family() {
    let scratch = new_scratch();
    scratch.write("run.sh", SHELL_WITH_DISABLE);
    scratch.write("src/attributes.rs", RUST_WITH_ATTRIBUTES);
    scratch.write("src/text.rs", RUST_WITH_ATTRIBUTE_TEXT);
    scratch.write_baseline("");
    scratch.git(&["add", "-A"]);
    scratch.git(&["commit", "-q", "-m", "one file per family"]);

    let (updated, output) = scratch.ratchet(&["--update"]);
    assert!(updated, "--update failed:\n{output}");
    let written = scratch.read(BASELINE_FILE);

    // Each lint of a multi-lint attribute counts separately.
    for line in [
        "shellcheck-disable/SC2086\t1",
        "expect/clippy::pedantic\t1",
        "expect/clippy::too_many_lines\t1",
        "expect/clippy::cast_possible_truncation\t1",
        "allow/dead_code\t1",
        "allow/missing_docs\t1",
    ] {
        assert!(
            written.contains(line),
            "the baseline does not record {line:?}:\n{written}"
        );
    }
    // The text fixture names clippy::unwrap_used and expect/dead_code only as data; the one real
    // allow(dead_code) is in the attributes fixture.
    for unwanted in ["allow/clippy::unwrap_used", "expect/dead_code", "reason"] {
        assert!(
            !written.contains(unwanted),
            "the baseline records {unwanted:?}, which is not a suppression:\n{written}"
        );
    }
}

/// A lint is located on the line that names it, not on the line that opens the attribute:
/// rustfmt puts each lint of a long attribute on its own line, and adding one there leaves the
/// opening line untouched.
#[test]
fn sees_a_lint_added_to_a_multi_line_expect() {
    let scratch = new_scratch();
    scratch.commit_initial(
        SCRATCH,
        RUST_WITH_MULTI_LINE_EXPECT,
        "expect/clippy::too_many_lines\t1",
    );
    scratch.write(SCRATCH, RUST_WITH_MULTI_LINE_EXPECT_EXTENDED);
    scratch.git(&["add", SCRATCH]);

    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        !accepted,
        "the ratchet accepted a lint added to an existing expect:\n{output}"
    );
    assert!(
        output.contains("scratch.rs:3\texpect/clippy::cast_possible_truncation"),
        "the output does not locate the added lint on its own line:\n{output}"
    );
}

/// A suppression in a Rust testdata fixture is neither counted nor treated as newly added: a
/// fixture is data.
#[test]
fn skips_rust_fixtures() {
    let scratch = new_scratch();
    scratch.commit_empty_baseline();
    scratch.write("crates/x/tests/testdata/fixture.rs", RUST_WITH_ONE);
    scratch.write("testdata/top.rs", RUST_WITH_ONE);
    scratch.git(&["add", "-A"]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(accepted, "the ratchet counted a fixture file:\n{output}");
}

/// The negative control of the test above: a file next to the excluded paths, but outside them,
/// and a shell script under testdata/ (the exclusion covers Rust files only) are counted like any
/// other, so an exclusion that grew too broad would fail here.
#[test]
fn counts_neighbors_of_excluded_paths() {
    let mut failures = Vec::new();
    for (name, content, key) in [
        (
            "crates/x/tests/testdatax/x.rs",
            RUST_WITH_ONE,
            "expect/clippy::too_many_lines",
        ),
        (
            "crates/x/tests/testdata/run.sh",
            SHELL_WITH_DISABLE,
            "shellcheck-disable/SC2086",
        ),
        (
            "scripts/bootstrap-mk-common.sh",
            SHELL_WITH_DISABLE,
            "shellcheck-disable/SC2086",
        ),
    ] {
        let scratch = new_scratch();
        scratch.commit_empty_baseline();
        scratch.write(name, content);
        scratch.git(&["add", "-A"]);
        let (accepted, output) = scratch.ratchet(&[]);
        if accepted || !output.contains(key) {
            failures.push(format!("the ratchet did not count {name}:\n{output}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The staged-diff rule sees whole files. The detector carries raw-string and block-comment state
/// across lines, so judging added lines on their own would both invent suppressions inside a
/// fixture and miss real ones after it closes.
#[test]
fn reads_the_staged_file_not_the_hunk() {
    // A line added inside a raw string is not a suppression.
    let scratch = new_scratch();
    scratch.commit_initial(SCRATCH, RUST_RAW_STRING_FIXTURE, "");
    scratch.write(SCRATCH, RUST_RAW_STRING_FIXTURE_EXTENDED);
    scratch.git(&["add", SCRATCH]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        accepted,
        "the ratchet read a raw-string line as an attribute:\n{output}"
    );

    // A real attribute added after the raw string still is. The staged-diff rule's own message is
    // asserted: the count rule refuses this edit too.
    let scratch = new_scratch();
    scratch.commit_initial(SCRATCH, RUST_RAW_STRING_FIXTURE, "");
    scratch.write(SCRATCH, RUST_RAW_STRING_FIXTURE_AND_ATTRIBUTE);
    scratch.git(&["add", SCRATCH]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        !accepted,
        "the ratchet accepted an added attribute:\n{output}"
    );
    assert!(
        output.contains("without touching .suppressions-baseline"),
        "the staged-diff rule did not see the attribute:\n{output}"
    );
}

/// The way out of the one refusal the counts cannot explain: a swap leaves every count where it
/// was, so --update has to produce a baseline that differs anyway, or there is nothing to stage.
#[test]
fn update_discharges_a_swap() {
    let scratch = new_scratch();
    scratch.commit_initial(SCRATCH, RUST_WITH_ONE, "");

    let (updated, output) = scratch.ratchet(&["--update"]);
    assert!(updated, "--update failed:\n{output}");
    scratch.git(&["add", BASELINE_FILE]);
    scratch.git(&["commit", "-q", "-m", "record the baseline"]);
    let before = scratch.read(BASELINE_FILE);

    scratch.write(SCRATCH, RUST_WITH_ONE_MOVED);
    scratch.git(&["add", SCRATCH]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        !accepted,
        "the ratchet accepted a swapped suppression:\n{output}"
    );

    let (updated, output) = scratch.ratchet(&["--update"]);
    assert!(updated, "--update failed:\n{output}");
    let after = scratch.read(BASELINE_FILE);
    assert_ne!(
        after, before,
        "--update left the baseline byte-identical, so there is nothing to stage"
    );

    scratch.git(&["add", BASELINE_FILE]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        accepted,
        "the ratchet still refuses the swap after the baseline was staged:\n{output}"
    );
}

/// Moving a file is not adding its suppressions: without rename detection a moved file would look
/// wholly new.
#[test]
fn allows_a_rename() {
    let scratch = new_scratch();
    scratch.commit_initial("old.rs", RUST_WITH_TWO, "expect/clippy::too_many_lines\t2");
    scratch.git(&["mv", "old.rs", "renamed.rs"]);

    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        accepted,
        "the ratchet read a moved file as newly added suppressions:\n{output}"
    );
}
