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

//! No gate passes on nothing: the Makefile, the make fragments, the pre-commit configuration and
//! every YAML file under `.github` carry none of the flags that turn a failing gate, or one with
//! nothing to check, into a pass, and every clippy run denies warnings. The check is
//! `repo_meta::vacuous::VacuousFlags` with the core and Rust sets; this repository needs no
//! exception.
//!
//! Extracted from swarm-device-access/meta/tests/vacuous_flags.rs at 8b4ab3a3e417.

use repo_meta::vacuous::{AllowedFragment, VacuousFlags};
use repo_meta::{report, strings};

/// The reviewed exceptions: none.
const ALLOWED: &[AllowedFragment<'static>] = &[];

fn flags() -> VacuousFlags {
    VacuousFlags::core().with_rust()
}

fn sample_problems(sample: &str) -> Vec<String> {
    strings(&flags().problems(&[("Makefile".to_owned(), sample.to_owned())], &[]))
}

#[test]
fn no_vacuous_flags() {
    let repo = meta::repo();
    let fragments = repo.files_under(".mk", &[".mk"]);
    assert!(
        !fragments.is_empty(),
        "no make fragments found under .mk/; the scan would check nothing"
    );
    let workflows = repo.files_under(".github", &[".yaml", ".yml"]);
    assert!(
        !workflows.is_empty(),
        "no YAML found under .github; the scan would check nothing"
    );
    let mut files = vec!["Makefile".to_owned(), ".pre-commit-config.yaml".to_owned()];
    files.extend(fragments);
    files.extend(workflows);

    let problems = flags().problems(&repo.read_all(&files), ALLOWED);
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn vacuous_flags_are_recognized() {
    for sample in [
        "\tcargo test ./... || true",
        "\tcargo test ./... ||:",
        "\tcargo test ./... || exit 0",
        "\t-cargo test",
        "\t@-cargo test",
        ".IGNORE: rust-test",
        "\t$(MAKE) -i rust-test",
        "\t$(MAKE) --ignore-errors rust-test",
        "        continue-on-error: true",
        "    if: false",
        "    if: ${{ false }}",
        "    if: 'false'",
        "    if: \"${{ false }}\"",
        "\tcargo test --workspace --locked --no-run",
        "        run: cargo clippy --workspace -- --cap-lints warn",
        "\tcargo clippy --workspace -- -A warnings",
        "\tcargo clippy --workspace -- --allow=warnings",
        "        entry: cargo test --workspace --locked -- --skip slow",
        "\tcargo test --workspace --exclude meta --locked",
        "\t$(CARGO) test --workspace --locked --no-run",
        "        run: cargo clippy --workspace --all-targets --locked",
        "\t$(CARGO) clippy --workspace --all-targets --all-features --locked",
        "        entry: cargo clippy --workspace --all-targets --locked",
    ] {
        assert!(
            !sample_problems(sample).is_empty(),
            "no vacuous flag found in {sample:?}"
        );
    }

    for sample in [
        "\t$(CARGO) clippy --workspace --all-targets --all-features --locked -- -D warnings",
        "        entry: cargo clippy --workspace --all-targets --locked -- --deny=warnings",
        "        name: cargo clippy",
        "      - name: Lint with clippy",
        "        entry: cargo test --locked -p meta --test docs",
    ] {
        let problems = sample_problems(sample);
        assert!(problems.is_empty(), "{sample:?} was flagged: {problems:?}");
    }
}

/// A hit names its file and line.
#[test]
fn vacuous_hit_numbering() {
    assert_eq!(
        sample_problems("all:\n\tcargo build\n\tcargo test || true\n"),
        ["Makefile:3: || true: cargo test || true"]
    );
}

/// An exception excuses its exact fragment in its one file only, a second flag on the same line is
/// still caught, and an exception that matches nothing is a problem.
#[test]
fn an_exception_is_exact_and_must_be_used() {
    let allowed = [AllowedFragment {
        file: ".github/workflows/release.yaml",
        fragment: "{ grep -E \"$TAG\" || true; }",
        reason: "grep exits 1 when no tag matches",
    }];
    let line = "            { grep -E \"$TAG\" || true; } | sort -V | tail -n1";
    let check = |file: &str, text: &str| {
        strings(&flags().problems(&[(file.to_owned(), text.to_owned())], &allowed))
    };
    assert_eq!(
        check(".github/workflows/release.yaml", line),
        Vec::<String>::new()
    );
    assert_eq!(
        check(".github/workflows/release.yaml", &format!("{line} || true")).len(),
        1
    );
    assert_eq!(
        check(".github/workflows/ci.yaml", line),
        [
            format!(".github/workflows/ci.yaml:1: || true: {}", line.trim()),
            ".github/workflows/release.yaml: the exception \"{ grep -E \\\"$TAG\\\" || true; }\" \
             (grep exits 1 when no tag matches) matches nothing; remove it"
                .to_owned(),
        ]
    );
}
