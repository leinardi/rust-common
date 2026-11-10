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

//! The pin check on manifest text: each refusal is shown on a planted violation, and manifests
//! that keep the pins pass.

use super::{RUST_COMMON_URL as URL, git_pins, root_git_pins};
use crate::scratch::ScratchRepo;
use crate::{report, strings};

const GOOD: &str = r#"
[workspace.dependencies]
alpha = { git = "https://github.com/leinardi/rust-common", tag = "v0.4.2", version = "0.4" }
beta = { git = "https://github.com/leinardi/rust-common.git/", tag = "v0.4.2", version = "0.4" }
serde = { git = "https://github.com/serde-rs/serde", rev = "abc" }
tokio = "1"
local = { workspace = true }
"#;

fn check(text: &str) -> Vec<String> {
    strings(&git_pins(&[("Cargo.toml", text)], URL))
}

fn one(dependency: &str) -> Vec<String> {
    check(&format!("[workspace.dependencies]\n{dependency}\n"))
}

#[test]
fn consistent_pins_pass_and_other_git_dependencies_are_ignored() {
    assert_eq!(check(GOOD), Vec::<String>::new());
}

#[test]
fn a_pin_by_rev_branch_or_nothing_is_refused() {
    let rev = r#"alpha = { git = "https://github.com/leinardi/rust-common", rev = "abc1234", version = "0.4" }"#;
    let branch = r#"alpha = { git = "https://github.com/leinardi/rust-common", branch = "main", version = "0.4" }"#;
    let bare = r#"alpha = { git = "https://github.com/leinardi/rust-common", version = "0.4" }"#;
    assert_eq!(
        one(rev),
        ["Cargo.toml: `alpha` is pinned by `rev` instead of `tag`; pin it with `tag = \"vX.Y.Z\"`"]
    );
    assert_eq!(
        one(branch),
        [
            "Cargo.toml: `alpha` is pinned by `branch` instead of `tag`; pin it with `tag = \"vX.Y.Z\"`"
        ]
    );
    assert_eq!(
        one(bare),
        ["Cargo.toml: `alpha` is pinned by no reference; pin it with `tag = \"vX.Y.Z\"`"]
    );
}

#[test]
fn tags_that_differ_are_refused() {
    let text =
        format!("{GOOD}gamma = {{ git = \"{URL}\", tag = \"v0.5.0\", version = \"0.5\" }}\n");
    assert_eq!(
        check(&text),
        [format!(
            "the dependencies on {URL} pin different tags, and cargo locks a checkout for each: \
             v0.4.2 (Cargo.toml: `alpha`, Cargo.toml: `beta`); v0.5.0 (Cargo.toml: `gamma`)"
        )]
    );
}

#[test]
fn a_version_that_is_not_the_tags_line_is_refused() {
    for version in ["0.3", "0.4.2", "^0.4", "0", "v0.4"] {
        let problems = one(&format!(
            "alpha = {{ git = \"{URL}\", tag = \"v0.4.2\", version = \"{version}\" }}"
        ));
        assert_eq!(
            problems,
            [format!(
                "Cargo.toml: `alpha` has version \"{version}\" but tag v0.4.2: the version must \
                 be \"0.4\", or a tag-only bump fails to select a version"
            )],
            "version {version}"
        );
    }
}

#[test]
fn a_missing_version_is_refused() {
    assert_eq!(
        one(&format!("alpha = {{ git = \"{URL}\", tag = \"v1.2.3\" }}")),
        ["Cargo.toml: `alpha` has tag v1.2.3 and no version: add `version = \"1.2\"`"]
    );
}

#[test]
fn a_tag_that_is_not_a_release_is_refused() {
    for tag in [
        "0.4.2",
        "v0.4",
        "v0.4.2.1",
        "v0.4.x",
        "v0.4.2-rc1",
        "v+1.2.3",
        "main",
        "",
    ] {
        assert_eq!(
            one(&format!(
                "alpha = {{ git = \"{URL}\", tag = \"{tag}\", version = \"0.4\" }}"
            )),
            [format!(
                "Cargo.toml: `alpha` has tag {tag:?}, which is not of the form vX.Y.Z"
            )],
            "tag {tag:?}"
        );
    }
}

#[test]
fn no_dependency_on_the_url_passes_on_nothing() {
    let message =
        format!("no dependency on {URL} in Cargo.toml: the pin check would pass on nothing");
    assert_eq!(
        check("[workspace.dependencies]\ntokio = \"1\"\n"),
        std::slice::from_ref(&message)
    );
    assert_eq!(check(""), [message]);
    assert_eq!(
        strings(&git_pins(&[], URL)),
        [format!(
            "no dependency on {URL} in no manifest: the pin check would pass on nothing"
        )]
    );
    // A dependency on another URL does not count for this one.
    assert_eq!(
        strings(&git_pins(
            &[("Cargo.toml", GOOD)],
            "https://example.com/other"
        ))
        .len(),
        1
    );
}

#[test]
fn a_manifest_that_does_not_parse_is_a_problem() {
    let problems = strings(&git_pins(
        &[("a/Cargo.toml", "[oops"), ("Cargo.toml", GOOD)],
        URL,
    ));
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(
        problems[0].starts_with("a/Cargo.toml: parse: "),
        "{problems:#?}"
    );
}

#[test]
fn a_renamed_dependency_is_judged_and_named_by_both_names() {
    assert_eq!(
        one(&format!(
            "alias = {{ git = \"{URL}\", package = \"alpha\", branch = \"main\" }}"
        )),
        [
            "Cargo.toml: `alias` (package `alpha`) is pinned by `branch` instead of `tag`; pin it \
          with `tag = \"vX.Y.Z\"`"
        ]
    );
    assert_eq!(
        one(&format!(
            "alias = {{ git = \"{URL}\", package = \"alpha\", tag = \"v0.4.2\", version = \"0.4\" }}"
        )),
        Vec::<String>::new()
    );
}

#[test]
fn member_and_other_dependency_tables_are_read_too() {
    let member = format!(
        "[dependencies]\nalpha = {{ git = \"{URL}\", rev = \"abc\" }}\n\n\
         [dev-dependencies]\nbeta = {{ git = \"{URL}\", tag = \"v0.4.2\", version = \"0.3\" }}\n\n\
         [build-dependencies]\ngamma = {{ git = \"{URL}\", tag = \"v0.9.0\", version = \"0.9\" }}\n"
    );
    let problems = strings(&git_pins(
        &[("Cargo.toml", GOOD), ("crates/m/Cargo.toml", &member)],
        URL,
    ));
    assert_eq!(problems.len(), 3, "{problems:#?}");
    assert!(problems[0].starts_with("crates/m/Cargo.toml: `alpha` is pinned by `rev`"));
    assert!(problems[1].starts_with("crates/m/Cargo.toml: `beta` has version \"0.3\""));
    assert!(problems[2].contains("pin different tags"), "{problems:#?}");
}

#[test]
fn the_root_manifest_is_read_through_the_repo() {
    let scratch = ScratchRepo::new();
    let problems = root_git_pins(&scratch.repo(), URL);
    assert_eq!(problems.len(), 1, "{}", report(&problems));

    scratch.write("Cargo.toml", GOOD);
    let problems = root_git_pins(&scratch.repo(), URL);
    assert!(problems.is_empty(), "{}", report(&problems));

    scratch.write(
        "Cargo.toml",
        &GOOD.replace(
            "tag = \"v0.4.2\", version = \"0.4\" }\nbeta",
            "rev = \"1\" }\nbeta",
        ),
    );
    assert_eq!(root_git_pins(&scratch.repo(), URL).len(), 1);
}
