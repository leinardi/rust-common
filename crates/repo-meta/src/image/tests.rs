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

//! The image pins' negative controls: on a sample Dockerfile shaped like the consumers', and on
//! byte-for-byte copies of bitgauge's Dockerfile at 600fc3905200 and swarm-rootless-docker's at
//! 3bb0fd8a615b (`tests/testdata`).
//!
//! Extracted from swarm-device-access/meta/tests/dockerfiles.rs at 8b4ab3a3e417 and
//! `bitgauge/meta/tests/dockerfiles.rs` at 600fc3905200.

use super::{BasePolicy, dockerfiles, is_dockerfile_path, stage_problems, syntax_problems};
use crate::scratch::ScratchRepo;
use crate::strings;

const DIGEST: &str = "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const BINARY: &str = "/usr/local/bin/app";

const BG_DOCKERFILE: &str = include_str!("../../tests/testdata/bitgauge.Dockerfile");
const SRD_DOCKERFILE: &str = include_str!("../../tests/testdata/srd.Dockerfile");

fn sample() -> String {
    format!(
        "# syntax=docker/dockerfile:1.7{DIGEST}\n\
         FROM --platform=$BUILDPLATFORM dhi.io/rust:1.98.1-debian13-dev AS build\n\
         ARG VERSION\nARG COMMIT\nARG DATE\nARG RUST_VERSION=1.98.1\n\
         RUN APP_VERSION=\"${{VERSION}}\" APP_COMMIT=\"${{COMMIT}}\" APP_DATE=\"${{DATE}}\" \
         cargo zigbuild --release --locked\n\
         FROM dhi.io/static:20250419 AS runtime\n\
         LABEL org.opencontainers.image.title=\"app\" \\\n  \
         org.opencontainers.image.description=\"An app.\" \\\n  \
         org.opencontainers.image.created=\"${{DATE}}\"\n\
         COPY --from=build /out/app {BINARY}\nUSER 0\nENTRYPOINT [\"{BINARY}\"]\n"
    )
}

#[test]
fn the_sample_passes_every_rule() {
    let content = sample();
    assert_eq!(syntax_problems("Dockerfile", &content, true), []);
    assert_eq!(
        stage_problems(
            "Dockerfile",
            &content,
            &["build", "runtime"],
            BasePolicy::DhiTagElseDigest
        ),
        []
    );
}

#[test]
fn syntax_and_stage_rules() {
    let content = sample();
    for (name, sample) in [
        ("no directive", content.replacen("# syntax=", "# note ", 1)),
        ("a tag alone", content.replace(DIGEST, "")),
        (
            "another frontend",
            content.replace("docker/dockerfile:1.7", "example/frontend:1.7"),
        ),
    ] {
        assert_eq!(
            syntax_problems("Dockerfile", &sample, true).len(),
            1,
            "{name}"
        );
    }
    let without = content.replacen("# syntax=", "# note ", 1);
    assert_eq!(syntax_problems("Dockerfile", &without, false), []);
    assert_eq!(
        syntax_problems("Dockerfile", &content.replace(DIGEST, ""), false).len(),
        1,
        "an optional directive is still held to the pin"
    );
    let policy = BasePolicy::DhiTagElseDigest;
    for (name, sample) in [
        (
            "a dhi.io base by digest",
            content.replace(":20250419", &format!(":20250419{DIGEST}")),
        ),
        ("a floating base", content.replace(":20250419", ":latest")),
        ("a renamed stage", content.replace("AS runtime", "AS final")),
        (
            "another registry by tag",
            content.replace("dhi.io/static", "docker.io/library/alpine"),
        ),
    ] {
        assert_eq!(
            stage_problems("Dockerfile", &sample, &["build", "runtime"], policy).len(),
            1,
            "{name}"
        );
    }
    assert_eq!(
        stage_problems("Dockerfile", "RUN x\n", &["build"], policy).len(),
        1,
        "a malformed file"
    );
    assert_eq!(
        strings(&stage_problems("Dockerfile", &content, &[], policy)),
        ["Dockerfile: no stage is wanted; the check would pass on nothing"]
    );
}

/// The policies: bitgauge's dhi.io-only refuses another registry even by digest, and
/// swarm-rootless-docker's tag-only takes its `docker:29-dind-rootless` and refuses a digest.
#[test]
fn base_policies() {
    let bg = ["web", "build", "runtime"];
    assert_eq!(
        stage_problems("Dockerfile", BG_DOCKERFILE, &bg, BasePolicy::DhiOnly),
        []
    );
    let hub = BG_DOCKERFILE.replace(
        "dhi.io/static:20250419",
        &format!("docker.io/library/alpine:3{DIGEST}"),
    );
    assert_eq!(
        stage_problems("Dockerfile", &hub, &bg, BasePolicy::DhiOnly).len(),
        1
    );
    assert_eq!(
        stage_problems("Dockerfile", &hub, &bg, BasePolicy::DhiTagElseDigest),
        []
    );
    let srd = ["build", "runtime"];
    assert_eq!(
        stage_problems("Dockerfile", SRD_DOCKERFILE, &srd, BasePolicy::TagOnly),
        []
    );
    assert_eq!(
        stage_problems(
            "Dockerfile",
            SRD_DOCKERFILE,
            &srd,
            BasePolicy::DhiTagElseDigest
        )
        .len(),
        1
    );
    for (name, image) in [
        ("by digest", format!("docker:29-dind-rootless{DIGEST}")),
        ("latest", "docker:latest".to_owned()),
        ("no tag", "docker".to_owned()),
    ] {
        let sample = SRD_DOCKERFILE.replace("docker:29-dind-rootless", &image);
        assert_eq!(
            stage_problems("Dockerfile", &sample, &srd, BasePolicy::TagOnly).len(),
            1,
            "{name}"
        );
    }
}

#[test]
fn the_listing_finds_every_dockerfile_but_ignored_ones() {
    let scratch = ScratchRepo::new();
    for path in [
        "deployments/docker/Dockerfile",
        "deployments/docker/Dockerfile.dockerignore",
        "test/x.Dockerfile",
        "test/Dockerfile.debug",
        ".github/hidden/Dockerfile",
        "target/debug/Dockerfile",
        "README.md",
    ] {
        scratch.write(path, "FROM scratch\n");
    }
    scratch.write(".gitignore", "target/\n");
    let found = dockerfiles(&scratch.repo());
    assert_eq!(
        found,
        [
            ".github/hidden/Dockerfile",
            "deployments/docker/Dockerfile",
            "test/Dockerfile.debug",
            "test/x.Dockerfile",
        ]
    );
    assert!(is_dockerfile_path("a/b/Dockerfile") && is_dockerfile_path("x.Dockerfile"));
    assert!(is_dockerfile_path("Dockerfile.debug"));
    assert!(!is_dockerfile_path("Dockerfile.md.dockerignore"));
    assert!(!is_dockerfile_path("Dockerfiles"));
}
