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

//! The reader's negative controls: each input has to come out as something the checks refuse.
//!
//! Extracted from swarm-device-access/meta/tests/dockerfiles.rs at 8b4ab3a3e417
//! (`parse_rejects_what_the_assertions_must_not_miss`) and bitgauge/meta/tests/dockerfiles.rs at
//! 600fc3905200 (`assert_reference_rules`, and the directive after another directive).

use super::{
    base_pinned, digest_pinned, from_dhi, labels, parse, repository, stage_named, syntax, tag,
    tag_pinned,
};

const DIGEST: &str = "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn tag_pins() {
    for (name, image) in [
        (
            "a variable tag",
            "dhi.io/rust:${RUST_LINE}-debian13-dev".to_owned(),
        ),
        ("a variable image", "$BASE".to_owned()),
        ("no tag", "dhi.io/static".to_owned()),
        ("latest", "dhi.io/static:latest".to_owned()),
        ("latest, upper-cased", "dhi.io/static:LATEST".to_owned()),
        ("a digest", format!("dhi.io/static:20250419{DIGEST}")),
        ("an empty tag", "dhi.io/static:".to_owned()),
        ("a stage name, not a ref", "build".to_owned()),
        ("a registry port, no tag", "registry:5000/app".to_owned()),
    ] {
        assert!(
            !tag_pinned(&image),
            "{name} ({image:?}) passes as tag-pinned"
        );
    }
    for image in [
        "dhi.io/static:20250419",
        "dhi.io/rust:1.98.1-debian13-dev",
        "registry:5000/app:1.2",
    ] {
        assert!(tag_pinned(image), "{image} does not pass as tag-pinned");
    }
}

#[test]
fn digest_pins() {
    for (name, image) in [
        ("a tag only", "docker/dockerfile:1.7".to_owned()),
        (
            "a short digest",
            "docker/dockerfile:1.7@sha256:0123".to_owned(),
        ),
        ("a digest with no tag", format!("docker/dockerfile{DIGEST}")),
        (
            "a variable tag",
            format!("docker/dockerfile:${{TAG}}{DIGEST}"),
        ),
        ("a variable image", format!("$BASE{DIGEST}")),
        ("a stage name, not a ref", "build".to_owned()),
    ] {
        assert!(
            !digest_pinned(&image),
            "{name} ({image:?}) passes as digest-pinned"
        );
    }
    assert!(digest_pinned(&format!("docker/dockerfile:1.7{DIGEST}")));
}

/// A dhi.io base by tag alone, any other by tag and digest; a dhi.io path under another registry
/// is another registry.
#[test]
fn base_pins_follow_the_registry() {
    for (image, want) in [
        ("dhi.io/static:20250419".to_owned(), true),
        (format!("dhi.io/static:20250419{DIGEST}"), false),
        ("dhi.io/static:latest".to_owned(), false),
        (format!("docker.io/library/alpine:3.23{DIGEST}"), true),
        ("docker.io/library/alpine:3.23".to_owned(), false),
        ("evil.example/dhi.io/static:20250419".to_owned(), false),
    ] {
        assert_eq!(base_pinned(&image), want, "base_pinned({image:?})");
    }
    assert!(from_dhi("dhi.io/rust:1.98.1-debian13-dev"));
    assert!(!from_dhi("docker.io/dhi.io/rust:1"));
}

#[test]
fn references_split_at_the_tag_not_the_port() {
    for (image, want_repository, want_tag) in [
        (
            "dhi.io/rust:1.98.1-debian13-dev",
            "dhi.io/rust",
            "1.98.1-debian13-dev",
        ),
        (
            "docker/dockerfile:1.7@sha256:00",
            "docker/dockerfile",
            "1.7",
        ),
        ("registry:5000/app:1", "registry:5000/app", "1"),
        ("registry:5000/app", "registry:5000/app", ""),
        ("alpine", "alpine", ""),
    ] {
        assert_eq!(repository(image), want_repository, "{image}");
        assert_eq!(tag(image), want_tag, "{image}");
    }
}

#[test]
fn stages_and_instructions() {
    let stages =
        parse("ARG BASE=x\nFROM a:1 AS one\nRUN echo \\\n  two\n# note\nfrom --platform=$BUILDPLATFORM b:2\nUSER 1:1\n")
            .unwrap();
    assert_eq!(stages.len(), 2, "{stages:?}");
    assert_eq!(stages[0].name, "one");
    assert_eq!(stages[0].joined(), "RUN echo two");
    assert!(stages[1].name.is_empty());
    assert_eq!(stages[1].platform, "$BUILDPLATFORM");
    assert_eq!(stages[1].image, "b:2");
    assert_eq!(stages[1].joined(), "USER 1:1");
    assert_eq!(
        stage_named("Dockerfile", &stages, "one").unwrap().image,
        "a:1"
    );
    assert_eq!(
        stage_named("Dockerfile", &stages, "two")
            .unwrap_err()
            .to_string(),
        "Dockerfile: has 0 stages named two, want one"
    );

    for (name, content) in [
        ("a continuation at the end", "FROM a:1\nRUN echo \\\n"),
        ("an instruction before FROM", "RUN echo\nFROM a:1\n"),
        ("a FROM it cannot read", "FROM a:1 b c d\n"),
    ] {
        assert!(parse(content).is_err(), "{name} parsed");
    }
    assert_eq!(
        parse("RUN echo\n").unwrap_err().to_string(),
        "malformed Dockerfile: RUN before the first FROM"
    );
}

#[test]
fn the_syntax_directive_is_read_where_buildkit_reads_it() {
    for (name, content) in [
        ("no directive", "FROM a:1\n".to_owned()),
        (
            "after a comment",
            format!("# note\n# syntax=docker/dockerfile:1.7{DIGEST}\n"),
        ),
        (
            "after a blank line",
            format!("\n# syntax=docker/dockerfile:1.7{DIGEST}\n"),
        ),
        (
            "after an instruction",
            format!("FROM a:1\n# syntax=docker/dockerfile:1.7{DIGEST}\n"),
        ),
        ("a directive with no value", "# syntax=\n".to_owned()),
    ] {
        let found = syntax(&content);
        assert!(
            found.is_none(),
            "{name}: read {found:?}, which BuildKit ignores"
        );
    }
    assert_eq!(
        syntax(&format!(
            "# check=error=true\n#Syntax = docker/dockerfile:1.7{DIGEST}\n"
        )),
        Some(format!("docker/dockerfile:1.7{DIGEST}")),
        "a directive after another directive"
    );
}

#[test]
fn labels_lose_their_quotes() {
    let stages = parse(
        "FROM a:1\nLABEL org.opencontainers.image.title=\"x y\" \\\n  org.opencontainers.image.version=${VERSION}\nLABEL a=b\n",
    )
    .unwrap();
    let found = labels(&stages[0]);
    assert_eq!(
        found.into_iter().collect::<Vec<_>>(),
        [
            ("a".to_owned(), "b".to_owned()),
            (
                "org.opencontainers.image.title".to_owned(),
                "x y".to_owned()
            ),
            (
                "org.opencontainers.image.version".to_owned(),
                "${VERSION}".to_owned()
            ),
        ]
    );
}
