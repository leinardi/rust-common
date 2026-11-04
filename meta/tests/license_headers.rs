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

//! Drives scripts/check-license-headers.sh through `repo_meta::scratch::HeaderHarness`: on this
//! repository, and on scratch repositories that hold each kind of file it must reject or let
//! through.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/license_headers.rs at c9e79afa8810, with
//! this repository's MIT OR Apache-2.0 notice, its governed kinds (no Dockerfile) and no testdata
//! exemption, which its checker does not have.

use repo_meta::report;
use repo_meta::scratch::{HeaderCase, HeaderHarness};

/// The dual-licence notice in a Rust block comment; its last required line is its eleventh.
const RUST_HEADER: &str = "/*
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

/// The same notice in hash comments after a shebang, with a year range.
const SHELL_HEADER: &str = "#!/usr/bin/env bash
#
# Copyright (c) 2025-2026 Roberto Leinardi
#
# Licensed under either of
#
#  * Apache License, Version 2.0 (LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0)
#  * MIT license (LICENSE-MIT or https://opensource.org/licenses/MIT)
#
# at your option.
#
# SPDX-License-Identifier: MIT OR Apache-2.0
echo ok
";

/// Another licence's notice with the right copyright line.
const MIT_HEADER: &str = "/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 */

fn x() {}
";

/// The Makefile every scratch repository holds unless a case brings its own: the checker reads the
/// make-common files from its `MK_COMMON_FILES`.
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
MK_COMMON_FILES       := help.mk password.mk pre-commit.mk rust.mk
";

const HARNESS: HeaderHarness<'static> = HeaderHarness {
    script: "scripts/check-license-headers.sh",
    base_files: &[("Makefile", MAKEFILE)],
    untracked_prefix: "untracked/",
};

fn run_cases(cases: &[HeaderCase]) {
    let problems = HARNESS.run_cases(&meta::repo(), cases);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The repository passes its own check, run with its own git environment: under the commit hook
/// it names the index being committed.
#[test]
fn repository_headers() {
    let problems = HARNESS.check_repository(&meta::repo());
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The notice on Rust files: present, missing, another licence, another holder, and the edges of
/// the 30-line window it must sit in. `RUST_HEADER`'s last required line is its eleventh: 19 lines
/// before it put it on line 30, the last line of the window, and one more pushes it out.
#[test]
fn header_cases_for_the_notice() {
    let other_holder = RUST_HEADER.replace("Roberto Leinardi", "Someone Else");
    let at_the_edge = format!("{}{RUST_HEADER}", "// filler\n".repeat(19));
    let past_the_edge = format!("{}{RUST_HEADER}", "// filler\n".repeat(20));
    run_cases(&[
        HeaderCase::new(
            "rust file with the header",
            &[("src/x.rs", RUST_HEADER)],
            true,
        ),
        HeaderCase::new(
            "rust file without a header",
            &[("src/x.rs", "fn x() {}\n")],
            false,
        ),
        HeaderCase::new(
            "another licence's header",
            &[("src/x.rs", MIT_HEADER)],
            false,
        ),
        HeaderCase::new(
            "another copyright holder",
            &[("meta/src/x.rs", &other_holder)],
            false,
        ),
        HeaderCase::new(
            "header ending on the last line of the window",
            &[("src/x.rs", &at_the_edge)],
            true,
        ),
        HeaderCase::new(
            "header ending one line past the window",
            &[("src/x.rs", &past_the_edge)],
            false,
        ),
        HeaderCase::new(
            "a rust testdata file is governed like any other",
            &[("crates/x/tests/testdata/fixture.rs", "fn x() {}\n")],
            false,
        ),
    ]);
}

/// Every other governed file kind with and without the header.
#[test]
fn header_cases_for_each_file_kind() {
    let makefile_without = MAKEFILE
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    run_cases(&[
        HeaderCase::new(
            "shell script with the header",
            &[("scripts/a.sh", SHELL_HEADER)],
            true,
        ),
        HeaderCase::new(
            "shell script without a header",
            &[("scripts/a.sh", "#!/usr/bin/env bash\necho\n")],
            false,
        ),
        HeaderCase::new(
            "local make fragment, no header",
            &[(".mk/verify.mk", "x:\n")],
            false,
        ),
        HeaderCase::new(
            "make fragment make-common ships but MK_COMMON_FILES does not list, no header",
            &[(".mk/docker.mk", "x:\n")],
            false,
        ),
        HeaderCase::new(
            "makefile without a header",
            &[("Makefile", &makefile_without)],
            false,
        ),
    ]);
}

/// What is never governed: untracked files, make-common's files, and Markdown, YAML and JSON;
/// and a file beside an exemption, which is.
#[test]
fn header_cases_for_the_exemptions() {
    run_cases(&[
        HeaderCase::new(
            "untracked file without a header",
            &[("untracked/scratch.rs", "fn x() {}\n")],
            true,
        ),
        HeaderCase::new(
            "upstream make fragment",
            &[
                (".mk/help.mk", "x:\n"),
                (".mk/pre-commit.mk", "y:\n"),
                (".mk/rust.mk", "z:\n"),
            ],
            true,
        ),
        HeaderCase::new(
            "bootstrap script",
            &[("scripts/bootstrap-mk-common.sh", "#!/bin/sh\n")],
            true,
        ),
        HeaderCase::new(
            "a neighbour of the bootstrap script is governed",
            &[("scripts/bootstrap-mk-common-local.sh", "#!/bin/sh\n")],
            false,
        ),
        HeaderCase::new(
            "markdown, yaml and json",
            &[
                ("README.md", "# x\n"),
                ("a.yaml", "a: 1\n"),
                ("b.json", "{}\n"),
            ],
            true,
        ),
    ]);
}

/// Negative control of the harness on this repository's checker: a case whose expectation is
/// wrong is reported, so a checker that accepted everything could not pass the cases above.
#[test]
fn a_wrong_expectation_is_reported() {
    let problems = HARNESS.run_cases(
        &meta::repo(),
        &[
            HeaderCase::new(
                "no header, expected to pass",
                &[("src/x.rs", "fn x() {}\n")],
                true,
            ),
            HeaderCase::new(
                "the header, expected to fail",
                &[("src/x.rs", RUST_HEADER)],
                false,
            ),
        ],
    );
    let names: Vec<&str> = problems
        .iter()
        .map(|problem| problem.location.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "no header, expected to pass",
            "the header, expected to fail"
        ]
    );
}
