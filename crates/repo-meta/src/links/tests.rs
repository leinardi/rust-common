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

//! The link check's negative controls, on scratch trees: missing files and anchors are found,
//! external links, code spans and fences are not checked, and the slugs follow GitHub's rules.
//!
//! Extracted from swarm-scheduler-exporter/meta/tests/docs.rs at c9e79afa8810
//! (`checker_finds_broken_links`, `slugs_follow_github`) and bitgauge/meta/tests/docs.rs at
//! 600fc3905200 (`slug`).

use std::fs;

use super::{Checker, anchors, is_markdown, slug, targets};
use crate::strings;

fn scratch(files: &[(&str, &str)]) -> tempfile::TempDir {
    let scratch = tempfile::tempdir().unwrap();
    for (name, content) in files {
        let path = scratch.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    scratch
}

#[test]
fn checker_finds_broken_links() {
    let page = [
        "# Title",
        "## 1. Section — `GET /x`",
        "[ok](b.md#heading-two) [ok](#1-section--get-x) [ok](https://example.com/missing)",
        "[broken file](missing.md) [broken anchor](b.md#nope) `[in code](missing-too.md)`",
        "```text",
        "[in a fence](missing-three.md)",
        "```",
    ]
    .join("\n");
    let tree = scratch(&[("a.md", &page), ("b.md", "# Heading Two\n")]);
    assert_eq!(
        strings(&Checker::new(tree.path()).check_file("a.md")),
        [
            r#"a.md:4: link "missing.md": missing.md does not exist"#,
            r#"a.md:4: link "b.md#nope": b.md has no heading or anchor #nope"#,
        ]
    );
}

/// Relative paths resolve against the page's directory; a root-relative one against the root; a
/// path above the root is refused; a reference definition, an HTML anchor and a repeated heading
/// count; a target that is not Markdown is not searched for a fragment.
#[test]
fn paths_and_anchors_resolve_as_github_does() {
    let page = [
        "# Same",
        "# Same",
        "<a id=\"custom\"></a>",
        "[up](../README.md) [rooted](/README.md#top) [sibling](other.md#same-1)",
        "[ref]: <other.md#custom>",
        "[code](../src/x.rs#L3) [away](../../outside.md) [self](#same-1) [none](#same-2)",
    ]
    .join("\n");
    let tree = scratch(&[
        ("README.md", "# Top\n"),
        ("docs/page.md", &page),
        (
            "docs/other.md",
            "# Same\n# Same\n<a name=\"custom\">x</a>\n",
        ),
        ("src/x.rs", "fn x() {}\n"),
    ]);
    let mut checker = Checker::new(tree.path());
    assert_eq!(
        strings(&checker.check_file("docs/page.md")),
        [
            r#"docs/page.md:6: link "../../outside.md" leaves the repository"#,
            r##"docs/page.md:6: link "#same-2": docs/page.md has no heading or anchor #same-2"##,
        ]
    );
    assert_eq!(
        strings(&checker.check_file("docs/missing.md")).len(),
        1,
        "an unreadable page is one problem"
    );
}

/// A copy of a page is checked as if it were the page, without touching the file on disk.
#[test]
fn check_text_reads_a_copy_as_the_file() {
    let tree = scratch(&[("docs/a.md", "[b](b.md#b)\n"), ("docs/b.md", "# B\n")]);
    let mut checker = Checker::new(tree.path());
    assert_eq!(checker.check_file("docs/a.md"), []);
    let broken = fs::read_to_string(tree.path().join("docs/a.md"))
        .unwrap()
        .replace("#b)", "#c)");
    assert_eq!(
        strings(&checker.check_text("docs/a.md", &broken)),
        [r#"docs/a.md:1: link "b.md#c": docs/b.md has no heading or anchor #c"#]
    );
}

/// A target is quoted the way Go's `%q` quotes it.
#[test]
fn a_target_is_quoted_as_go_does() {
    let tree = scratch(&[("a.md", "[x](<sp ace\"q.md>)\n")]);
    assert_eq!(
        strings(&Checker::new(tree.path()).check_file("a.md")),
        [r#"a.md:1: link "sp ace\"q.md": sp ace"q.md does not exist"#]
    );
}

#[test]
fn slugs_follow_github() {
    for (heading, want) in [
        ("1. Test (Home) — `/`", "1-test-home--"),
        ("5. `GET /api/v1/info`", "5-get-apiv1info"),
        ("5.1 Implementation", "51-implementation"),
        (
            "Why the Build Happens Before the Tag",
            "why-the-build-happens-before-the-tag",
        ),
        (
            "11.2 Path A fallback — HTTP probe",
            "112-path-a-fallback--http-probe",
        ),
        ("5. History Module (`idb`)", "5-history-module-idb"),
        (
            "**Bold** and _emphasis_ with snake_case",
            "bold-and-emphasis-with-snake_case",
        ),
        ("[Linked](other.md) heading", "linked-heading"),
        ("2.2 Stateless Server", "22-stateless-server"),
        ("Tags, Moves and `X.Y.Z`", "tags-moves-and-xyz"),
        ("Ünïcode Lëtters Stay", "ünïcode-lëtters-stay"),
        (
            "Required Checks and Fork Pull Requests",
            "required-checks-and-fork-pull-requests",
        ),
        (
            "6. `compat.networkquality.max_upload_bytes`",
            "6-compatnetworkqualitymax_upload_bytes",
        ),
        (
            "💡 What is “Swarm Scheduler Exporter”?",
            "-what-is-swarm-scheduler-exporter",
        ),
        // Go lower-cases rune by rune: a dotted capital I is a plain i, a final sigma stays σ.
        ("İSTANBUL ΟΔΟΣ", "istanbul-οδοσ"),
    ] {
        assert_eq!(slug(heading), want, "{heading:?}");
    }
}

#[test]
fn readers() {
    assert_eq!(
        targets("[a](x.md) `[b](y.md)` ![c](<z w.png> \"title\")"),
        ["x.md", "z w.png"]
    );
    assert_eq!(
        targets("   [ref]: https://example.com"),
        ["https://example.com"]
    );
    let found = anchors("# A\n```\n# Not\n```\n# A\n");
    assert!(found.contains("a") && found.contains("a-1") && !found.contains("not"));
    assert!(is_markdown("docs/x.md") && !is_markdown("docs/x.MD"));
}
