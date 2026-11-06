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

//! The image checks' negative controls: on a sample Dockerfile shaped like the consumers', and on
//! byte-for-byte copies of bitgauge's Dockerfile, Makefile, make fragments and droast.toml at
//! 600fc3905200 and swarm-rootless-docker's Dockerfile at 3bb0fd8a615b (`tests/testdata`), with
//! bitgauge's own mutations, each of which must fail with bitgauge's parameters.
//!
//! Extracted from swarm-device-access/meta/tests/dockerfiles.rs at 8b4ab3a3e417
//! (`build_stage_rules`, `stamp_rules`, `runtime_rules`, `platform_rules`,
//! `label_policy_refuses_a_missing_label`) and `bitgauge/meta/tests/dockerfiles.rs` at
//! 600fc3905200 (the mutations).

use super::{
    BasePolicy, BuildBase, BuildStage, Download, RuntimeStage, STAMP_FIELDS, StampSources, Stamps,
    build_stage_problems, dockerfiles, is_dockerfile_path, label_policy_problems, label_problems,
    only_dockerfile_problems, platform_problems, runtime_problems, stage_problems, syntax_problems,
};
use crate::dockerfile::{Stage, parse, stage_named};
use crate::scratch::ScratchRepo;
use crate::strings;

const DIGEST: &str = "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const BINARY: &str = "/usr/local/bin/app";

const BG_DOCKERFILE: &str = include_str!("../../tests/testdata/bitgauge.Dockerfile");
const BG_MAKEFILE: &str = include_str!("../../tests/testdata/bitgauge.Makefile");
const BG_RUST_MK: &str = include_str!("../../tests/testdata/bitgauge.rust.mk");
const BG_RELEASE_MK: &str = include_str!("../../tests/testdata/bitgauge.rust-release.mk");
const BG_DROAST: &str = include_str!("../../tests/testdata/bitgauge.droast.toml");
const SRD_DOCKERFILE: &str = include_str!("../../tests/testdata/srd.Dockerfile");
const SSE_DOCKERFILE: &str = include_str!("../../tests/testdata/sse.Dockerfile");

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

const BUILD: BuildStage<'static> = BuildStage {
    platform: "$BUILDPLATFORM",
    base: BuildBase::Repository("dhi.io/rust"),
    required: &[
        "ARG VERSION",
        "ARG RUST_VERSION=",
        "cargo zigbuild --release --locked",
    ],
    downloads: &[],
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

fn stage(content: &str, name: &str) -> Stage {
    let stages = parse(content).unwrap();
    stage_named("Dockerfile", &stages, name).unwrap().clone()
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
    let build = stage(&content, "build");
    let runtime = stage(&content, "runtime");
    assert_eq!(build_stage_problems(&build, &BUILD), []);
    assert_eq!(runtime_problems(&runtime, &runtime_stage()), []);
    assert_eq!(
        label_problems(
            &runtime,
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

/// A stage on the target platform, from another base, or without `--locked` is each refused; a
/// stage that downloads nothing passes with no download rule, and a download no rule names fails.
#[test]
fn build_stage_rules() {
    let content = sample();
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
    ] {
        assert_ne!(sample, content, "{name}");
        let problems = build_stage_problems(&stage(&sample, "build"), &BUILD);
        assert_eq!(problems.len(), 1, "{name}: {problems:?}");
    }
    for (name, line) in [
        (
            "an undeclared download",
            "RUN curl -fsSL -o /tmp/x https://example.invalid/x",
        ),
        (
            "a download from a variable",
            "ARG EXTRA_URL=https://example.invalid/x\nRUN curl -fsSL -o /tmp/x \"${EXTRA_URL}\"",
        ),
        (
            "a wget after an assignment",
            "RUN true && URL=x wget -q -O /tmp/x \"${URL}\"",
        ),
        ("an added archive", "ADD https://example.invalid/x /tmp/x"),
        ("an added variable", "ADD ${EXTRA_URL} /tmp/x"),
    ] {
        let sample = content.replace("RUN APP_VERSION", &format!("{line}\nRUN APP_VERSION"));
        let problems = build_stage_problems(&stage(&sample, "build"), &BUILD);
        assert!(!problems.is_empty(), "{name} passes");
    }
    let installs_curl = content.replace(
        "RUN APP_VERSION",
        "RUN apt-get install -y curl\nRUN APP_VERSION",
    );
    assert_eq!(
        build_stage_problems(&stage(&installs_curl, "build"), &BUILD),
        [],
        "installing curl downloads nothing by itself"
    );
}

/// The `RUST_RELEASE_<name> := <value>` assignments of bitgauge's release fragment.
fn release_value(name: &str) -> String {
    let prefix = format!("RUST_RELEASE_{name} := ");
    BG_RELEASE_MK
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("no {name}"))
        .trim()
        .to_owned()
}

fn arms(prefix: &str, arms: &[(&str, &str)]) -> Vec<(String, String, String)> {
    arms.iter()
        .map(|(arch, asset)| {
            (
                (*arch).to_owned(),
                (*asset).to_owned(),
                release_value(&format!("{prefix}{asset}")),
            )
        })
        .collect()
}

/// bitgauge's three downloads, from its release fragment.
fn bg_downloads() -> Vec<Download<'static>> {
    vec![
        Download {
            tool: "zig",
            url: "https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz",
            platform_arg: "BUILDARCH",
            version_arg: "ZIG_VERSION",
            version: release_value("ZIG_VERSION"),
            asset_var: "zig_arch",
            sha_var: "zig_sha256",
            arms: arms("ZIG_SHA256_", &[("amd64", "x86_64"), ("arm64", "aarch64")]),
        },
        Download {
            tool: "cargo-zigbuild",
            url: "https://github.com/rust-cross/cargo-zigbuild/releases/download/v${CARGO_ZIGBUILD_VERSION}/cargo-zigbuild-${zigbuild_asset}.tar.xz",
            platform_arg: "BUILDARCH",
            version_arg: "CARGO_ZIGBUILD_VERSION",
            version: release_value("CARGO_ZIGBUILD_VERSION"),
            asset_var: "zigbuild_asset",
            sha_var: "zigbuild_sha256",
            arms: arms(
                "CARGO_ZIGBUILD_SHA256_",
                &[
                    ("amd64", "x86_64-unknown-linux-musl"),
                    ("arm64", "aarch64-unknown-linux-gnu"),
                ],
            ),
        },
        Download {
            tool: "the musl rust-std",
            url: "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION}-${triple}.tar.xz",
            platform_arg: "TARGETARCH",
            version_arg: "RUST_VERSION",
            version: release_value("RUST_STD_VERSION"),
            asset_var: "triple",
            sha_var: "std_sha256",
            arms: arms(
                "RUST_STD_SHA256_",
                &[
                    ("amd64", "x86_64-unknown-linux-musl"),
                    ("arm64", "aarch64-unknown-linux-musl"),
                ],
            ),
        },
    ]
}

fn bg_build_problems(content: &str) -> Vec<String> {
    let downloads = bg_downloads();
    let rules = BuildStage {
        platform: "$BUILDPLATFORM",
        base: BuildBase::Exact("dhi.io/rust:1.98.1-debian13-dev"),
        required: &[
            "cargo zigbuild --release --locked --target \"${triple}\" --bin bitgauge",
            "cp \"target/${triple}/release/bitgauge\" /out/bitgauge",
        ],
        downloads: &downloads,
    };
    let web = BuildStage {
        platform: "$BUILDPLATFORM",
        base: BuildBase::Repository("dhi.io/node"),
        required: &[],
        downloads: &[],
    };
    let mut problems = strings(&build_stage_problems(&stage(content, "build"), &rules));
    problems.extend(strings(&build_stage_problems(&stage(content, "web"), &web)));
    problems
}

const BG_STAMPS: Stamps<'static> = Stamps {
    prefix: "BITGAUGE",
    fields: &STAMP_FIELDS,
    version_file: "src/buildinfo.rs",
    build_command: "cargo zigbuild",
};

const BG_VERSION_RS: &str = "option_env!(\"BITGAUGE_VERSION\") option_env!(\"BITGAUGE_COMMIT\") \
                             option_env!(\"BITGAUGE_DATE\")";

fn bg_stamp_problems(dockerfile: &str, makefile: &str, rust_mk: &str, release_mk: &str) -> usize {
    let build = stage(dockerfile, "build");
    BG_STAMPS
        .problems(&StampSources {
            version_rs: BG_VERSION_RS,
            build: &build,
            makefile,
            rust_mk,
            release: Some((release_mk, "rust_static_build")),
        })
        .len()
}

/// swarm-scheduler-exporter's build stage, whose downloads are bitgauge's, passes with them; an
/// unchecked extra download in a RUN that also fetches a listed URL fails, as SSE's own
/// per-stage count refuses it.
#[test]
fn an_extra_download_beside_a_listed_one_fails() {
    let downloads = bg_downloads();
    let rules = BuildStage {
        platform: "$BUILDPLATFORM",
        base: BuildBase::Exact("dhi.io/rust:1.98.1-debian13-dev"),
        required: &[
            "cargo zigbuild --release --locked --target \"${triple}\" --bin swarm-scheduler-exporter",
        ],
        downloads: &downloads,
    };
    let check = |content: &str| build_stage_problems(&stage(content, "build"), &rules);
    assert_eq!(check(SSE_DOCKERFILE), []);
    let zig_check = "\t&& echo \"${zig_sha256}  /tmp/zig.tar.xz\" | sha256sum -c - \\\n";
    assert!(SSE_DOCKERFILE.contains(zig_check));
    for extra in [
        "\t&& curl -fsSL -o /tmp/x https://example.invalid/x \\\n",
        "\t&& curl -fsSL -o /tmp/x \"${EXTRA_URL}\" \\\n",
        // Shapes where no command starts right before the curl: SSE's per-stage count still
        // sees each one.
        "\t&& case \"${BUILDARCH}\" in amd64) curl -fsSL -o /tmp/x https://example.invalid/x ;; esac \\\n",
        "\t&& if curl -fsSL -o /tmp/x https://example.invalid/x; then :; fi \\\n",
        "\t&& /usr/bin/curl -fsSL -o /tmp/x https://example.invalid/x \\\n",
        "\t&& env curl -fsSL -o /tmp/x https://example.invalid/x \\\n",
        "\t&& command curl -fsSL -o /tmp/x https://example.invalid/x \\\n",
        "\t&& { curl -fsSL -o /tmp/x https://example.invalid/x; } \\\n",
        "\t&& x=`curl -fsSL https://example.invalid/x` \\\n",
        "\t&& sh -c \"curl -fsSL -o /tmp/x https://example.invalid/x\" \\\n",
        "\t&& echo https://example.invalid/x | xargs curl -fsSL -o /tmp/x \\\n",
    ] {
        let sample = SSE_DOCKERFILE.replacen(zig_check, &format!("{zig_check}{extra}"), 1);
        assert!(!check(&sample).is_empty(), "{extra:?} passes");
    }
}

/// bitgauge's files pass with bitgauge's parameters.
#[test]
fn bitgauge_passes_with_its_parameters() {
    assert_eq!(syntax_problems("Dockerfile", BG_DOCKERFILE, true), []);
    assert_eq!(bg_build_problems(BG_DOCKERFILE), Vec::<String>::new());
    assert_eq!(
        bg_stamp_problems(BG_DOCKERFILE, BG_MAKEFILE, BG_RUST_MK, BG_RELEASE_MK),
        0
    );
    let runtime = RuntimeStage {
        repository: "dhi.io/static",
        forbidden: &["RUN", "SHELL", "VOLUME", "WORKDIR", "ADD", "ENV"],
        exactly: &[
            ("USER", "65532:65532"),
            ("EXPOSE", "8080"),
            ("ENTRYPOINT", "[\"/bitgauge\"]"),
            ("COPY", "--from=build /out/bitgauge /bitgauge"),
        ],
    };
    assert_eq!(
        runtime_problems(&stage(BG_DOCKERFILE, "runtime"), &runtime),
        []
    );
    assert_eq!(
        label_policy_problems("Dockerfile", BG_DOCKERFILE, "droast.toml", BG_DROAST),
        []
    );
}

/// bitgauge's own mutations of its build stages, each refused with its parameters.
#[test]
fn bitgauge_build_mutations_fail() {
    let aarch64_std = release_value("RUST_STD_SHA256_aarch64-unknown-linux-musl");
    let x86_64_std = release_value("RUST_STD_SHA256_x86_64-unknown-linux-musl");
    for (name, from, to) in [
        (
            "a rust tag off the channel",
            "dhi.io/rust:1.98.1-",
            "dhi.io/rust:1.97.0-",
        ),
        ("the alpine variant", "-debian13-dev", "-alpine3.23-dev"),
        (
            "a rust-std off the image's toolchain",
            "ARG RUST_VERSION=1.98.1",
            "ARG RUST_VERSION=1.0.0",
        ),
        (
            "the build stage emulated",
            "FROM --platform=$BUILDPLATFORM dhi.io/rust",
            "FROM dhi.io/rust",
        ),
        (
            "the web stage emulated",
            "--platform=$BUILDPLATFORM dhi.io/node",
            "dhi.io/node",
        ),
        (
            "the web stage from another base",
            "dhi.io/node:",
            "dhi.io/bun:",
        ),
        ("a changed rust-std sha256", &x86_64_std, "x"),
        ("a changed aarch64 rust-std sha256", &aarch64_std, "00"),
        (
            "another zig release",
            "ARG ZIG_VERSION=0.17.0",
            "ARG ZIG_VERSION=0.0.1",
        ),
        (
            "another cargo-zigbuild release",
            "ARG CARGO_ZIGBUILD_VERSION=0.24.0",
            "ARG CARGO_ZIGBUILD_VERSION=0.0.1",
        ),
        (
            "rust-std from elsewhere",
            "https://static.rust-lang.org/dist/",
            "https://example.com/dist/",
        ),
        (
            "zig unchecked",
            "echo \"${zig_sha256}  /tmp/zig.tar.xz\" | sha256sum -c -",
            "true",
        ),
        (
            "zig for the target platform",
            "case \"${BUILDARCH}\" in \\\n\t\tamd64) zig_arch",
            "case \"${TARGETARCH}\" in \\\n\t\tamd64) zig_arch",
        ),
        (
            "swapped rust-std arms",
            "amd64) triple=x86_64-unknown-linux-musl",
            "arm64) triple=x86_64-unknown-linux-musl",
        ),
        (
            "a dropped aarch64 rust-std arm",
            "\t\tarm64) triple=aarch64-unknown-linux-musl;",
            "\t\t*) triple=aarch64-unknown-linux-musl;",
        ),
        (
            "a junk riscv64 arm",
            "\t\t*) echo \"no Rust target",
            "\t\triscv64) triple=riscv64gc-unknown-linux-musl; std_sha256=00 ;; \\\n\t\t*) echo \"no Rust target",
        ),
        ("an unlocked build", "--release --locked", "--release"),
        (
            "the binary left in target",
            "cp \"target/${triple}/release/bitgauge\" /out/bitgauge",
            "true",
        ),
    ] {
        assert!(BG_DOCKERFILE.contains(from), "{name}: {from:?} is gone");
        let sample = BG_DOCKERFILE.replacen(from, to, 1);
        assert!(
            !bg_build_problems(&sample).is_empty(),
            "{name} passes with bitgauge's parameters"
        );
    }
}

/// bitgauge's mutations of the Dockerfile side of its stamps, each refused.
#[test]
fn bitgauge_stamp_mutations_fail() {
    let docker = |from: &str, to: &str| {
        assert!(BG_DOCKERFILE.contains(from), "{from:?} is gone");
        bg_stamp_problems(
            &BG_DOCKERFILE.replacen(from, to, 1),
            BG_MAKEFILE,
            BG_RUST_MK,
            BG_RELEASE_MK,
        )
    };
    assert!(
        docker("BITGAUGE_COMMIT=\"${COMMIT}\"", "") > 0,
        "a missing stamp"
    );
    assert!(
        docker("BITGAUGE_DATE=\"${DATE}\"", "BITGAUGE_DATE=\"${COMMIT}\"") > 0,
        "a stamp from the wrong argument"
    );
    assert!(
        docker("ARG DATE=\"unknown\"\n", "") > 0,
        "an undeclared stamp argument"
    );
    assert!(
        docker("\t\tBITGAUGE_VERSION=\"${VERSION}\" \\\n", "\t\ttrue \\\n") > 0,
        "the version stamp dropped from the building RUN"
    );
    assert!(
        docker(
            "RUN --mount=type=cache,target=/root/.cargo/registry",
            "RUN true cargo zigbuild\nRUN --mount=type=cache,target=/root/.cargo/registry"
        ) > 0,
        "a second building RUN"
    );
}

/// bitgauge's mutations of its Makefile's stamp prefix, each refused, and the assignments that keep
/// the value accepted.
#[test]
fn bitgauge_prefix_mutations_fail() {
    let make = |makefile: &str, rust_mk: &str, release_mk: &str| {
        bg_stamp_problems(BG_DOCKERFILE, makefile, rust_mk, release_mk)
    };
    for (name, makefile) in [
        (
            "no prefix",
            BG_MAKEFILE.replace("RUST_STAMP_PREFIX := BITGAUGE", ""),
        ),
        (
            "a trailing space",
            BG_MAKEFILE.replace(
                "RUST_STAMP_PREFIX := BITGAUGE",
                "RUST_STAMP_PREFIX := BITGAUGE ",
            ),
        ),
        (
            "a later assignment wins",
            format!("{BG_MAKEFILE}RUST_STAMP_PREFIX := OTHER\n"),
        ),
        (
            "only a comment",
            BG_MAKEFILE.replace(
                "RUST_STAMP_PREFIX := BITGAUGE",
                "# RUST_STAMP_PREFIX := BITGAUGE",
            ),
        ),
    ] {
        assert!(BG_MAKEFILE != makefile, "{name}");
        assert!(make(&makefile, BG_RUST_MK, BG_RELEASE_MK) > 0, "{name}");
    }
    for (name, line) in [
        ("an override", "override RUST_STAMP_PREFIX := OTHER"),
        ("an exported one", "export RUST_STAMP_PREFIX := OTHER"),
        ("an append", "RUST_STAMP_PREFIX += OTHER"),
    ] {
        assert!(
            make(&format!("{BG_MAKEFILE}{line}\n"), BG_RUST_MK, BG_RELEASE_MK) > 0,
            "{name} wins and is read"
        );
    }
    assert_eq!(
        make(
            &BG_MAKEFILE.replace(
                "RUST_STAMP_PREFIX := BITGAUGE",
                "export RUST_STAMP_PREFIX := BITGAUGE"
            ),
            BG_RUST_MK,
            BG_RELEASE_MK
        ),
        0,
        "export keeps the value"
    );
    assert_eq!(
        make(
            &format!("{BG_MAKEFILE}RUST_STAMP_PREFIX ?= OTHER\n"),
            BG_RUST_MK,
            BG_RELEASE_MK
        ),
        0,
        "a later ?= does not change the value"
    );
}

/// bitgauge's mutations of the make side of its stamps, each refused: the Makefile's prefix,
/// rust.mk's assignments and recipe, and the release recipe, a stamp left only in a comment too.
#[test]
fn bitgauge_make_stamp_mutations_fail() {
    let make = |makefile: &str, rust_mk: &str, release_mk: &str| {
        bg_stamp_problems(BG_DOCKERFILE, makefile, rust_mk, release_mk)
    };
    for (name, from, to) in [
        (
            "the commit from another variable",
            "_COMMIT=\"$(GIT_COMMIT)\"",
            "_COMMIT=\"$(GIT_VERSION)\"",
        ),
        ("no stamps at all", "RUST_VERSION_ENV =", "RUST_ENV ="),
        (
            "an unstamped build",
            "\t$(RUST_VERSION_ENV) $(CARGO) build --workspace",
            "\t$(CARGO) build --workspace",
        ),
    ] {
        assert!(BG_RUST_MK.contains(from), "{name}");
        assert!(
            make(
                BG_MAKEFILE,
                &BG_RUST_MK.replacen(from, to, 1),
                BG_RELEASE_MK
            ) > 0,
            "{name}"
        );
    }
    let commented = format!(
        "{}\n# rust-build:\n#\t$(RUST_VERSION_ENV) $(CARGO) build\n",
        BG_RUST_MK.replace(
            "\t$(RUST_VERSION_ENV) $(CARGO) build --workspace",
            "\t$(CARGO) build --workspace"
        )
    );
    assert!(
        make(BG_MAKEFILE, &commented, BG_RELEASE_MK) > 0,
        "a stamped recipe in a comment"
    );
    assert!(
        make(
            BG_MAKEFILE,
            &format!("{BG_RUST_MK}\nrust-build:\n\t$(CARGO) build\n"),
            BG_RELEASE_MK
        ) > 0,
        "a later rust-build rule wins"
    );
    let from = "rust_static_build = RUSTFLAGS=\"$(RUST_RELEASE_RUSTFLAGS)\" $(RUST_VERSION_ENV) \\";
    assert!(BG_RELEASE_MK.contains(from));
    let unstamped = BG_RELEASE_MK.replacen(
        from,
        "rust_static_build = RUSTFLAGS=\"$(RUST_RELEASE_RUSTFLAGS)\" \\\n# $(RUST_VERSION_ENV)",
        1,
    );
    assert!(
        make(BG_MAKEFILE, BG_RUST_MK, &unstamped) > 0,
        "the static build unstamped, the variable left in a comment"
    );
}

/// A stamp dropped from each place, another prefix, a stamp from another variable and a build
/// recipe that does not stamp are each a problem, on the sample.
#[test]
fn stamp_rules() {
    let stamps = Stamps {
        prefix: "APP",
        fields: &STAMP_FIELDS,
        version_file: "src/version.rs",
        build_command: "cargo zigbuild",
    };
    let version_rs = "stamp(option_env!(\x22APP_VERSION\x22))\n\
                      stamp(option_env!(\x22APP_COMMIT\x22))\n\
                      stamp(option_env!(\x22APP_DATE\x22))\n";
    let build = stage(&sample(), "build");
    let makefile = "BIN_NAME ?= x\nRUST_STAMP_PREFIX := APP\n";
    let fragment = "RUST_VERSION_ENV = $(if $(strip $(RUST_STAMP_PREFIX)), \\\n\
                    \x20 $(RUST_STAMP_PREFIX)_VERSION=\x22$(GIT_VERSION)\x22 \\\n\
                    \x20 $(RUST_STAMP_PREFIX)_COMMIT=\x22$(GIT_COMMIT)\x22 \\\n\
                    \x20 $(RUST_STAMP_PREFIX)_DATE=\x22$(BUILD_DATE)\x22)\n\
                    \n\
                    rust-build: ## Build\n\
                    \t$(RUST_VERSION_ENV) $(CARGO) build --workspace\n";
    let check = |version_rs: &str, build: &Stage, makefile: &str, fragment: &str| {
        stamps.problems(&StampSources {
            version_rs,
            build,
            makefile,
            rust_mk: fragment,
            release: None,
        })
    };
    assert_eq!(check(version_rs, &build, makefile, fragment), []);
    let problems = check(
        &version_rs.replace("APP_DATE", "APP_WHEN"),
        &stage(&sample().replace("${COMMIT}", "none"), "build"),
        makefile,
        &fragment.replace("$(RUST_STAMP_PREFIX)_VERSION=", "VERSION="),
    );
    assert_eq!(problems.len(), 3, "{problems:#?}");
    for broken in [
        makefile.replace(":= APP", ":= OTHER"),
        makefile.replace("RUST_STAMP_PREFIX := APP\n", ""),
        makefile.replace("RUST_STAMP_PREFIX :=", "# RUST_STAMP_PREFIX :="),
        makefile.replace(":= APP", ":= APP "),
    ] {
        assert_eq!(
            check(version_rs, &build, &broken, fragment).len(),
            1,
            "{broken:?}"
        );
    }
    for broken in [
        fragment.replace("\t$(RUST_VERSION_ENV) $(CARGO)", "\t$(CARGO)"),
        fragment.replace("$(GIT_COMMIT)", "$(OTHER)"),
    ] {
        assert_eq!(
            check(version_rs, &build, makefile, &broken).len(),
            1,
            "{broken:?}"
        );
    }
    let no_fields = Stamps {
        fields: &[],
        ..stamps
    };
    assert_eq!(
        strings(&no_fields.problems(&StampSources {
            version_rs,
            build: &build,
            makefile,
            rust_mk: fragment,
            release: None,
        })),
        ["no stamp field; the check would pass on nothing"]
    );
}

/// A healthcheck, an exposed port, a non-root user, a shell command, a pinned platform and another
/// base are each refused; an extra or missing label too, and an empty label list.
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
        (
            "a shell base",
            content.replace("FROM dhi.io/static:", "FROM dhi.io/busybox:"),
        ),
    ] {
        assert_ne!(sample, content, "{name}");
        assert_eq!(
            runtime_problems(&stage(&sample, "runtime"), &runtime_stage()).len(),
            1,
            "{name}"
        );
    }
    let runtime = stage(&content, "runtime");
    assert_eq!(
        strings(&label_problems(
            &runtime,
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
            &runtime,
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
    let nothing = RuntimeStage {
        repository: "dhi.io/static",
        forbidden: &[],
        exactly: &[],
    };
    assert_eq!(runtime_problems(&runtime, &nothing).len(), 1, "empty rules");
    assert_eq!(
        strings(&label_problems(&runtime, &[])),
        ["no label is wanted of the runtime stage; the check would pass on nothing"]
    );
}

#[test]
fn platform_rules() {
    let release = ".github/workflows/release.yaml";
    let good = "env:\n  PLATFORMS: linux/amd64,linux/arm64\n";
    let check = |workflows: &[(&str, &str)]| {
        let workflows: Vec<(String, String)> = workflows
            .iter()
            .map(|(file, text)| ((*file).to_owned(), (*text).to_owned()))
            .collect();
        platform_problems(
            &workflows,
            release,
            "linux/amd64,linux/arm64",
            &["linux/arm/v", "linux/386"],
        )
    };
    assert_eq!(check(&[(release, good)]), []);
    let third = good.replace("linux/arm64", "linux/arm64,linux/s390x");
    let arm = format!("{good}  other: linux/arm/v7\n");
    for (name, workflows) in [
        ("a third platform", vec![(release, third.as_str())]),
        ("an arm/v7 image", vec![(release, arm.as_str())]),
        (
            "only an expression",
            vec![(release, "      platforms: ${{ env.PLATFORMS }}\n")],
        ),
        (
            "the declaration in another file",
            vec![(release, "jobs: {}\n"), (".github/workflows/ci.yaml", good)],
        ),
        (
            "the release workflow not checked",
            vec![(".github/workflows/ci.yaml", good)],
        ),
    ] {
        assert_eq!(check(&workflows).len(), 1, "{name}");
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
        (
            "a missing label lowered to a warning",
            content.clone(),
            droast.replace(
                "strict-labels = true\n",
                "strict-labels = true\n\n[severity-overrides]\nDF074 = \"warning\"\n",
            ),
        ),
    ] {
        assert_eq!(check(&dockerfile, &policy).len(), 1, "{name}");
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
    assert_eq!(
        only_dockerfile_problems(&found, "deployments/docker/Dockerfile").len(),
        1
    );
    assert_eq!(
        only_dockerfile_problems(&found[1..2], "deployments/docker/Dockerfile"),
        []
    );
    assert!(is_dockerfile_path("a/b/Dockerfile") && is_dockerfile_path("x.Dockerfile"));
    assert!(is_dockerfile_path("Dockerfile.debug"));
    assert!(!is_dockerfile_path("Dockerfile.md.dockerignore"));
    assert!(!is_dockerfile_path("Dockerfiles"));
}
