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

//! The allowlist's negative controls, for both forms. The lint names are spelled in pieces, so
//! this file does not mention them itself.
//!
//! Extracted from swarm-device-access/meta/tests/unsafe_allowlist.rs at 8b4ab3a3e417
//! (`a_mention_outside_the_allowlist_is_refused`,
//! `the_allowlisted_module_uses_only_the_file_level_expect`) and
//! `bitgauge/meta/tests/lint_exemptions.rs` at 600fc3905200 (`the_allowlisted_exemptions_pass`,
//! `an_exemption_outside_the_allowlist_is_refused`, `a_similar_lint_name_is_not_a_group`,
//! `a_wrapped_expect_is_read_whole`, `a_bracket_in_a_reason_does_not_hide_a_later_exemption`,
//! `a_repeated_exemption_is_refused`).

use std::collections::BTreeMap;

use super::{Exemption, LintAllowlist, allowlist_problems};
use crate::strings;

const UNSAFE: &str = concat!("unsafe", "_code");
const METHODS: &str = concat!("clippy::disallowed", "_methods");
const MACROS: &str = concat!("clippy::disallowed", "_macros");
const GROUPS: [&str; 2] = [concat!("clippy::", "style"), concat!("clippy::", "all")];

const BPF: &str = "src/cgroup/bpf_sys.rs";

const LINTS: [LintAllowlist<'static>; 3] = [
    LintAllowlist {
        lint: UNSAFE,
        exemptions: &[Exemption::FileLevel { file: BPF }],
    },
    LintAllowlist {
        lint: METHODS,
        exemptions: &[
            Exemption::Item {
                file: "src/healthcheck.rs",
                item: "connect",
            },
            Exemption::Item {
                file: "src/test_support.rs",
                item: "dial_loopback",
            },
            Exemption::Item {
                file: "src/clock.rs",
                item: "SystemClock",
            },
        ],
    },
    LintAllowlist {
        lint: MACROS,
        exemptions: &[],
    },
];

fn check_all(entries: &[(&str, &str)]) -> Vec<String> {
    let files: BTreeMap<String, String> = entries
        .iter()
        .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
        .collect();
    strings(&allowlist_problems(&files, &LINTS, &GROUPS))
}

/// The problems but the unused exemptions, which a scan of one file always has; those are
/// [`an_unused_exemption_is_refused`]'s.
fn check(entries: &[(&str, &str)]) -> Vec<String> {
    check_all(entries)
        .into_iter()
        .filter(|problem| !problem.ends_with("matches nothing; remove it"))
        .collect()
}

fn allowlisted_files() -> Vec<(&'static str, String)> {
    let methods = expect_line(METHODS);
    vec![
        (
            BPF,
            format!("//! Docs.\n\n#![expect({UNSAFE}, reason = \"raw bpf(2) calls\")]\n"),
        ),
        (
            "src/healthcheck.rs",
            format!("{methods}\nfn connect(address: SocketAddr) {{}}\n"),
        ),
        (
            "src/test_support.rs",
            format!(
                "/// Dials.\n{methods}\n#[must_use]\npub(crate) async fn dial_loopback() {{}}\n"
            ),
        ),
        (
            "src/clock.rs",
            format!("{methods}\nimpl Clock for SystemClock {{}}\n"),
        ),
        ("src/lib.rs", "pub mod server;\n".to_owned()),
    ]
}

fn expect_line(lint: &str) -> String {
    format!("#[expect({lint}, reason = \"the one exemption\")]")
}

#[test]
fn the_allowlisted_exemptions_pass() {
    let files = allowlisted_files();
    let entries: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, text)| (*path, text.as_str()))
        .collect();
    assert_eq!(check_all(&entries), Vec::<String>::new());
}

/// An exemption no file uses is refused, so it cannot outlive the code it excuses; no file and no
/// lint are refused too.
#[test]
fn an_unused_exemption_is_refused() {
    let files = allowlisted_files();
    let entries: Vec<(&str, &str)> = files
        .iter()
        .filter(|(path, _)| *path != "src/clock.rs")
        .map(|(path, text)| (*path, text.as_str()))
        .collect();
    assert_eq!(
        check_all(&entries),
        [format!(
            "src/clock.rs: the exemption for {METHODS} on SystemClock matches nothing; remove it"
        )]
    );
    assert_eq!(
        strings(&allowlist_problems(&BTreeMap::new(), &LINTS, &GROUPS)),
        ["no file to scan; the allowlist check would pass on nothing"]
    );
    let files: BTreeMap<String, String> = [("src/lib.rs".to_owned(), String::new())].into();
    assert_eq!(
        strings(&allowlist_problems(&files, &[], &GROUPS)),
        ["no allowlisted lint; the allowlist check would pass on nothing"]
    );
}

/// Every way to name the file-level lint outside its form or its file is refused.
#[test]
fn the_file_level_form_is_the_only_one() {
    for text in [
        format!("#![expect({UNSAFE}, reason = \"x\")]\n"),
        format!("#[allow({UNSAFE})]\nfn run() {{}}\n"),
        format!("#[expect(\n    clippy::too_many_lines,\n    {UNSAFE},\n    reason = \"x\"\n)]\n"),
        format!("#![cfg_attr(test, expect({UNSAFE}, reason = \"x\"))]\n"),
        format!("// {UNSAFE} is fine here, says a comment\n"),
    ] {
        assert_eq!(
            check(&[("src/cgroup/v2.rs", &text)]).len(),
            1,
            "accepted:\n{text}"
        );
    }
    let good = format!("//! Docs.\n\n#![expect({UNSAFE}, reason = \"raw bpf(2) calls\")]\n");
    for text in [
        format!("#![allow({UNSAFE})]\n"),
        format!("#![expect({UNSAFE})]\n"),
        format!("#![expect({UNSAFE}, reason = \"\")]\n"),
        format!("#[expect({UNSAFE}, reason = \"item level\")]\nfn run() {{}}\n"),
        format!("{good}#[allow({UNSAFE})]\nfn run() {{}}\n"),
        format!("{good}{good}"),
        format!("#![expect({UNSAFE}, reason = \"{UNSAFE}\")]\n"),
    ] {
        assert!(
            !check(&[(BPF, &text)]).is_empty(),
            "accepted in {BPF}:\n{text}"
        );
    }
    let two = format!("fn run() {{}}\n#[allow({UNSAFE})]\n#[allow({UNSAFE})]\nfn walk() {{}}\n");
    let problems = check(&[("src/x.rs", &two)]);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[0].starts_with("src/x.rs:2: names "),
        "{problems:?}"
    );
    assert!(
        problems[1].starts_with("src/x.rs:3: names "),
        "{problems:?}"
    );
}

/// The same item-level expect elsewhere, on another item, for another lint, at file level, as an
/// allow, without a reason, in a `cfg_attr`, in a comment, and any group mention are refused.
#[test]
fn an_item_exemption_outside_the_allowlist_is_refused() {
    let methods = expect_line(METHODS);
    let cases = [
        (
            "src/handlers/upload.rs",
            format!("{methods}\nfn store() {{ std::fs::write(\"x\", \"y\").ok(); }}\n"),
        ),
        (
            "src/healthcheck.rs",
            format!("{methods}\nfn probe() {{}}\n"),
        ),
        (
            "src/healthcheck.rs",
            format!("{}\nfn connect() {{}}\n", expect_line(MACROS)),
        ),
        (
            "src/healthcheck.rs",
            format!("#![expect({METHODS}, reason = \"crate-wide\")]\n"),
        ),
        (
            "src/healthcheck.rs",
            format!("#[allow({METHODS}, reason = \"no\")]\nfn connect() {{}}\n"),
        ),
        (
            "src/healthcheck.rs",
            format!("#[expect({METHODS})]\nfn connect() {{}}\n"),
        ),
        (
            "src/main.rs",
            format!("#![cfg_attr(test, expect({MACROS}))]\n"),
        ),
        ("src/lib.rs", format!("// {METHODS} is lifted elsewhere\n")),
        (
            "src/lib.rs",
            format!("#![expect({}, reason = \"crate-wide\")]\n", GROUPS[0]),
        ),
        (
            "src/handlers/upload.rs",
            format!(
                "#[expect({}, reason = \"one item\")]\nfn store() {{}}\n",
                GROUPS[1]
            ),
        ),
        (
            "src/healthcheck.rs",
            format!("#[allow({})]\nfn connect() {{}}\n", GROUPS[0]),
        ),
    ];
    for (path, text) in cases {
        let problems = check(&[(path, &text)]);
        assert_eq!(problems.len(), 1, "{path}:\n{text}\n{problems:?}");
    }
}

#[test]
fn a_similar_lint_name_is_not_a_group() {
    let text = format!(
        "#[expect({}_x, reason = \"x\")]\n#[expect({}d, reason = \"x\")]\nfn plain() {{}}\n",
        GROUPS[1], GROUPS[0]
    );
    assert_eq!(check(&[("src/lib.rs", &text)]), Vec::<String>::new());
}

#[test]
fn a_wrapped_expect_is_read_whole() {
    let text = format!(
        "#[expect(\n    {METHODS},\n    reason = \"the one exemption\"\n)]\npub(crate) async fn \
         dial_loopback() {{}}\n"
    );
    assert_eq!(
        check(&[("src/test_support.rs", &text)]),
        Vec::<String>::new()
    );
    let elsewhere = text.replace("dial_loopback", "dial_anywhere");
    let problems = check(&[("src/test_support.rs", &elsewhere)]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with("src/test_support.rs:1:"),
        "{problems:?}"
    );
}

#[test]
fn a_bracket_in_a_reason_does_not_hide_a_later_exemption() {
    let text = format!(
        "#[expect(\n    {METHODS},\n    reason = \"the one exemption (see below\"\n)]\n\
         pub(crate) fn dial_loopback() {{}}\n\n#[expect({METHODS}, reason = \"x\")]\n\
         fn store() {{}}\n"
    );
    let problems = check(&[("src/test_support.rs", &text)]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with("src/test_support.rs:7:"),
        "{problems:?}"
    );
}

#[test]
fn a_repeated_exemption_is_refused() {
    let methods = expect_line(METHODS);
    let text = format!("{methods}\nfn connect() {{}}\n{methods}\nfn connect() {{}}\n");
    assert_eq!(
        check(&[("src/healthcheck.rs", &text)]),
        [format!(
            "src/healthcheck.rs: lifts {METHODS} on connect 2 times; at most once"
        )]
    );
}
