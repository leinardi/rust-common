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

//! The image checks' negative controls, on a sample Dockerfile shaped like the consumers'.
//!
//! Extracted from swarm-device-access/meta/tests/dockerfiles.rs at 8b4ab3a3e417
//! (`build_stage_rules`, `stamp_rules`, `runtime_rules`, `platform_rules`,
//! `label_policy_refuses_a_missing_label`).

use std::fs;

use super::{
    BuildStage, RuntimeStage, STAMP_FIELDS, Stamps, build_stage_problems, dockerfiles,
    is_dockerfile_path, label_policy_problems, label_problems, only_dockerfile_problems,
    platform_problems, runtime_problems, stage_problems, syntax_problems,
};
use crate::dockerfile::{parse, stage_named};
use crate::{Repo, strings};

const DIGEST: &str = "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const BINARY: &str = "/usr/local/bin/app";

fn sample() -> String {
    format!(
        "# syntax=docker/dockerfile:1.7{DIGEST}\n\
         FROM --platform=$BUILDPLATFORM dhi.io/rust:1.98.1-debian13-dev AS build\n\
         ARG VERSION\nARG COMMIT\nARG DATE\nARG RUST_VERSION=1.98.1\n\
         RUN curl -fsSL -o /tmp/zig.tar.xz https://example.invalid/zig.tar.xz \\\n  \
         && echo \"${{zig_sha256}}  /tmp/zig.tar.xz\" | sha256sum -c - \\\n  \
         && APP_VERSION=\"${{VERSION}}\" APP_COMMIT=\"${{COMMIT}}\" APP_DATE=\"${{DATE}}\" \
         cargo zigbuild --release --locked\n\
         FROM dhi.io/static:20250419 AS runtime\n\
         LABEL org.opencontainers.image.title=\"app\" \\\n  \
         org.opencontainers.image.description=\"An app.\" \\\n  \
         org.opencontainers.image.created=\"${{DATE}}\"\n\
         COPY --from=build /out/app {BINARY}\nUSER 0\nENTRYPOINT [\"{BINARY}\"]\n"
    )
}

const BUILD: BuildStage<'static> = BuildStage {
    platform: "$BUILDPLATFORM",
    repository: "dhi.io/rust",
    required: &[
        "ARG VERSION",
        "ARG RUST_VERSION=",
        "cargo zigbuild --release --locked",
    ],
};

fn runtime_stage() -> RuntimeStage<'static> {
    RuntimeStage {
        repository: "dhi.io/static",
        forbidden: &[
            "RUN",
            "SHELL",
            "VOLUME",
            "WORKDIR",
            "ADD",
            "ENV",
            "HEALTHCHECK",
            "EXPOSE",
        ],
        exactly: &[
            ("USER", "0"),
            ("ENTRYPOINT", "[\"/usr/local/bin/app\"]"),
            ("COPY", "--from=build /out/app /usr/local/bin/app"),
        ],
    }
}

#[test]
fn the_sample_passes_every_rule() {
    let content = sample();
    assert_eq!(syntax_problems("Dockerfile", &content), []);
    assert_eq!(
        stage_problems("Dockerfile", &content, &["build", "runtime"]),
        []
    );
    let stages = parse(&content).unwrap();
    let build = stage_named("Dockerfile", &stages, "build").unwrap();
    let runtime = stage_named("Dockerfile", &stages, "runtime").unwrap();
    assert_eq!(build_stage_problems(build, &BUILD), []);
    assert_eq!(runtime_problems(runtime, &runtime_stage()), []);
    assert_eq!(
        label_problems(
            runtime,
            &[
                ("org.opencontainers.image.title", Some("app")),
                ("org.opencontainers.image.description", None),
                ("org.opencontainers.image.created", Some("${DATE}")),
            ]
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
        assert_eq!(syntax_problems("Dockerfile", &sample).len(), 1, "{name}");
    }
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
            stage_problems("Dockerfile", &sample, &["build", "runtime"]).len(),
            1,
            "{name}"
        );
    }
    assert_eq!(
        stage_problems("Dockerfile", "RUN x\n", &["build"]).len(),
        1,
        "a malformed file"
    );
}

/// A stage on the target platform, from another base, without `--locked`, or with a download
/// nobody checks is each refused.
#[test]
fn build_stage_rules() {
    let content = sample();
    let unchecked = content.replacen(" | sha256sum -c -", "", 1);
    for (name, sample) in [
        (
            "the target platform",
            content.replace("FROM --platform=$BUILDPLATFORM dhi.io", "FROM dhi.io"),
        ),
        (
            "another base",
            content.replace("dhi.io/rust:", "docker.io/library/rust:"),
        ),
        (
            "an unlocked build",
            content.replace(
                "cargo zigbuild --release --locked",
                "cargo zigbuild --release",
            ),
        ),
        ("an unchecked download", unchecked),
    ] {
        assert_ne!(sample, content, "{name}");
        let stages = parse(&sample).unwrap();
        let build = stage_named("Dockerfile", &stages, "build").unwrap();
        assert_eq!(build_stage_problems(build, &BUILD).len(), 1, "{name}");
    }
}

/// A stamp dropped from each place, another prefix, a stamp from another variable and a build
/// recipe that does not stamp are each a problem.
#[test]
fn stamp_rules() {
    let stamps = Stamps {
        prefix: "APP",
        fields: &STAMP_FIELDS,
        version_file: "src/version.rs",
    };
    let version_rs = "stamp(option_env!(\x22APP_VERSION\x22))\n\
                      stamp(option_env!(\x22APP_COMMIT\x22))\n\
                      stamp(option_env!(\x22APP_DATE\x22))\n";
    let stages = parse(&sample()).unwrap();
    let build = stage_named("Dockerfile", &stages, "build")
        .unwrap()
        .joined();
    let makefile = "BIN_NAME ?= x\nRUST_STAMP_PREFIX := APP\n";
    let fragment = "RUST_VERSION_ENV = $(if $(strip $(RUST_STAMP_PREFIX)), \\\n\
                    \x20 $(RUST_STAMP_PREFIX)_VERSION=\x22$(GIT_VERSION)\x22 \\\n\
                    \x20 $(RUST_STAMP_PREFIX)_COMMIT=\x22$(GIT_COMMIT)\x22 \\\n\
                    \x20 $(RUST_STAMP_PREFIX)_DATE=\x22$(BUILD_DATE)\x22)\n\
                    \n\
                    rust-build: ## Build\n\
                    \t$(RUST_VERSION_ENV) $(CARGO) build --workspace\n";
    assert_eq!(stamps.problems(version_rs, &build, makefile, fragment), []);
    let problems = stamps.problems(
        &version_rs.replace("APP_DATE", "APP_WHEN"),
        &build.replace("${COMMIT}", "none"),
        makefile,
        &fragment.replace("$(RUST_STAMP_PREFIX)_VERSION=", "VERSION="),
    );
    assert_eq!(problems.len(), 3, "{problems:#?}");
    for broken in [
        makefile.replace(":= APP", ":= OTHER"),
        makefile.replace("RUST_STAMP_PREFIX := APP\n", ""),
        makefile.replace("RUST_STAMP_PREFIX :=", "# RUST_STAMP_PREFIX :="),
    ] {
        assert_eq!(
            stamps.problems(version_rs, &build, &broken, fragment).len(),
            1,
            "{broken:?}"
        );
    }
    for broken in [
        fragment.replace("\t$(RUST_VERSION_ENV) $(CARGO)", "\t$(CARGO)"),
        fragment.replace("$(GIT_COMMIT)", "$(OTHER)"),
    ] {
        assert_eq!(
            stamps.problems(version_rs, &build, makefile, &broken).len(),
            1,
            "{broken:?}"
        );
    }
}

/// A healthcheck, an exposed port, a non-root user, a shell command, a pinned platform and an
/// extra or missing label are each refused.
#[test]
fn runtime_rules() {
    let content = sample();
    let entrypoint = format!("ENTRYPOINT [\x22{BINARY}\x22]\n");
    for (name, sample) in [
        (
            "a healthcheck",
            content.replace(
                &entrypoint,
                &format!("HEALTHCHECK CMD [\"{BINARY}\"]\n{entrypoint}"),
            ),
        ),
        (
            "an exposed port",
            content.replace(&entrypoint, &format!("EXPOSE 9090\n{entrypoint}")),
        ),
        (
            "a non-root user",
            content.replace("\nUSER 0\n", "\nUSER 65532:65532\n"),
        ),
        (
            "a shell command",
            content.replace(&entrypoint, &format!("RUN true\n{entrypoint}")),
        ),
        (
            "a pinned platform",
            content.replace(
                "FROM dhi.io/static:",
                "FROM --platform=linux/amd64 dhi.io/static:",
            ),
        ),
    ] {
        assert_ne!(sample, content, "{name}");
        let stages = parse(&sample).unwrap();
        let runtime = stage_named("Dockerfile", &stages, "runtime").unwrap();
        assert_eq!(
            runtime_problems(runtime, &runtime_stage()).len(),
            1,
            "{name}"
        );
    }
    let stages = parse(&content).unwrap();
    let runtime = stage_named("Dockerfile", &stages, "runtime").unwrap();
    assert_eq!(
        strings(&label_problems(
            runtime,
            &[("org.opencontainers.image.title", Some("app"))]
        )),
        [
            "the runtime stage carries the label org.opencontainers.image.created, which is not \
             one of the wanted labels",
            "the runtime stage carries the label org.opencontainers.image.description, which is \
             not one of the wanted labels",
        ]
    );
    assert_eq!(
        label_problems(
            runtime,
            &[
                ("org.opencontainers.image.title", Some("other")),
                ("org.opencontainers.image.description", None),
                ("org.opencontainers.image.created", Some("${DATE}")),
                ("org.opencontainers.image.licenses", Some("MIT")),
            ]
        )
        .len(),
        2
    );
}

#[test]
fn platform_rules() {
    let good = "env:\n  PLATFORMS: linux/amd64,linux/arm64\n".to_owned();
    let check = |content: &str| {
        platform_problems(
            &[("a.yaml".to_owned(), content.to_owned())],
            "linux/amd64,linux/arm64",
            &["linux/arm/v", "linux/386"],
        )
    };
    assert_eq!(check(&good), []);
    for (name, sample) in [
        (
            "a third platform",
            good.replace("linux/arm64", "linux/arm64,linux/s390x"),
        ),
        ("an arm/v7 image", format!("{good}  other: linux/arm/v7\n")),
        (
            "only an expression",
            "      platforms: ${{ env.PLATFORMS }}\n".to_owned(),
        ),
    ] {
        assert_eq!(check(&sample).len(), 1, "{name}");
    }
}

#[test]
fn label_policy_rules() {
    let content = sample();
    let droast = "strict-labels = true\n\n[required-labels]\n\
                  \"org.opencontainers.image.title\" = \".+\"\n\
                  \"org.opencontainers.image.description\" = \".+\"\n\
                  \"org.opencontainers.image.created\" = \".+\"\n";
    let check = |dockerfile: &str, droast: &str| {
        label_policy_problems("Dockerfile", dockerfile, "droast.toml", droast)
    };
    assert_eq!(check(&content, droast), []);
    for (name, dockerfile, policy) in [
        (
            "a Dockerfile without a label",
            content.replace(" \\\n  org.opencontainers.image.created=\"${DATE}\"", ""),
            droast.to_owned(),
        ),
        (
            "a policy without a label",
            content.clone(),
            droast.replace("\"org.opencontainers.image.created\" = \".+\"\n", ""),
        ),
        (
            "a policy without strict-labels",
            content.clone(),
            droast.replace("strict-labels = true", "strict-labels = false"),
        ),
    ] {
        assert_eq!(check(&dockerfile, &policy).len(), 1, "{name}");
    }
}

#[test]
fn the_walk_finds_every_dockerfile_but_generated_ones() {
    let scratch = tempfile::tempdir().unwrap();
    for path in [
        "deployments/docker/Dockerfile",
        "test/x.Dockerfile",
        "target/debug/Dockerfile",
        ".claude/worktrees/w/Dockerfile",
        "dist/Dockerfile",
        "README.md",
    ] {
        let file = scratch.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "FROM scratch\n").unwrap();
    }
    let found = dockerfiles(&Repo::scratch(scratch.path())).unwrap();
    assert_eq!(
        found,
        ["deployments/docker/Dockerfile", "test/x.Dockerfile"]
    );
    assert_eq!(
        only_dockerfile_problems(&found, "deployments/docker/Dockerfile").len(),
        1
    );
    assert_eq!(
        only_dockerfile_problems(&found[..1], "deployments/docker/Dockerfile"),
        []
    );
    assert!(is_dockerfile_path("a/b/Dockerfile") && is_dockerfile_path("x.Dockerfile"));
    assert!(!is_dockerfile_path("Dockerfile.md"));
}
