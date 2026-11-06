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

//! The tooling pins: every version written in more than one place is one value everywhere it is
//! written, so a bump in one file cannot leave another building, linting or testing with the old
//! one.
//!
//! - [`one_value`]: the general rule, over [`PinSource`]s (a file and the pattern whose first
//!   group is the pin there) and values found elsewhere.
//! - The Rust release: [`rust_release`] reads the exact release from `rust-toolchain.toml`;
//!   [`rust_version_problems`] holds the Cargo `rust-version` to it ([`RustVersion::Exact`]) or to
//!   its line ([`RustVersion::Line`], for a workspace that lets an earlier patch release build);
//!   [`image_rust_problems`] holds the image's `dhi.io/rust` tag and `ARG RUST_VERSION` to it;
//!   [`ci_toolchain_problems`] holds every Rust toolchain CI names to it; [`rust_std_problems`]
//!   holds the release fragment's musl standard libraries to it and to the toolchain's targets.
//! - The release toolchain: [`release_pin_problems`] holds the versions and archive digests the
//!   Dockerfile repeats to the release fragment, the one record of them;
//!   [`zigbuild_action_problems`] holds the setup-zigbuild action to that record.
//! - [`shellcheck_problems`] and [`trivy_problems`]: the ShellCheck the hook wraps and CI
//!   installs, and the one exact Trivy every scanning workflow installs.
//!
//! Extracted from swarm-device-access/meta/tests/toolchain.rs at 8b4ab3a3e417 (the checks as
//! functions of the contents, the exact release everywhere, the image's Rust, the zigbuild and
//! rust-std rules, Trivy's), swarm-scheduler-exporter/meta/tests/toolchain.rs at c9e79afa8810
//! (`expect_one_value`, the Rust line, the zigbuild pins against the Dockerfile, Trivy as one
//! value across workflows) and bitgauge/meta/tests/toolchain.rs at 600fc3905200 (optional sources
//! and extra values, every toolchain action and the toolchains scripts name, the release pins as
//! maps of archives with no stray digest).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::yaml::{ActionStep, Value, action_steps, scalar_text};
use crate::{Problem, Repo, case_arm, pattern, regex};

/// One place a pin is written: a file, its content (`None` when it cannot be read), and the
/// pattern whose first group is the pin there; every match counts. A required source that is
/// unreadable or writes no match is a problem; an optional one may be absent.
#[derive(Clone, Debug)]
pub struct PinSource {
    /// The file, relative to the root.
    pub file: String,
    /// Its content.
    pub content: Option<String>,
    /// The pattern; its first group is the pin.
    pub pattern: Regex,
    /// Whether the source may be absent or write nothing.
    pub optional: bool,
}

impl PinSource {
    /// A required source with the given content.
    ///
    /// # Panics
    ///
    /// `pattern` does not compile ([`crate::pattern`]).
    #[must_use]
    pub fn new(file: &str, content: &str, pattern: &str) -> Self {
        Self {
            file: file.to_owned(),
            content: Some(content.to_owned()),
            pattern: crate::pattern(pattern),
            optional: false,
        }
    }

    /// A required source read from `repo`; unreadable, it is a problem.
    ///
    /// # Panics
    ///
    /// `pattern` does not compile ([`crate::pattern`]).
    #[must_use]
    pub fn read(repo: &Repo, file: &str, pattern: &str) -> Self {
        Self {
            file: file.to_owned(),
            content: repo.try_read(file).ok(),
            pattern: crate::pattern(pattern),
            optional: false,
        }
    }

    /// The same source, allowed to be absent or to write nothing.
    #[must_use]
    pub fn optional(self) -> Self {
        Self {
            optional: true,
            ..self
        }
    }
}

/// One pin found, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinValue {
    /// The value.
    pub value: String,
    /// The file it is written in.
    pub file: String,
}

impl PinValue {
    /// `value`, found in `file`.
    #[must_use]
    pub fn new(value: &str, file: &str) -> Self {
        Self {
            value: value.to_owned(),
            file: file.to_owned(),
        }
    }
}

/// What is wrong unless every required source writes the pin and all the values found, `extra`
/// included, are one value: a problem per required source that is unreadable or writes nothing,
/// one listing the values when there are several, and one when there is neither a source nor a
/// value.
#[must_use]
pub fn one_value(what: &str, sources: &[PinSource], extra: &[PinValue]) -> Vec<Problem> {
    if sources.is_empty() && extra.is_empty() {
        return vec![Problem::new(format!(
            "{what}: no source and no value; the check would pass on nothing"
        ))];
    }
    let mut problems = Vec::new();
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for value in extra {
        found
            .entry(value.value.clone())
            .or_default()
            .push(value.file.clone());
    }
    for source in sources {
        let Some(content) = &source.content else {
            if !source.optional {
                problems.push(Problem::new(format!(
                    "{what}: {} cannot be read",
                    source.file
                )));
            }
            continue;
        };
        let mut matched = false;
        for captures in source.pattern.captures_iter(content) {
            matched = true;
            found
                .entry(captures[1].to_owned())
                .or_default()
                .push(source.file.clone());
        }
        if !matched && !source.optional {
            problems.push(Problem::new(format!(
                "{what}: {} does not write it ({})",
                source.file, source.pattern
            )));
        }
    }
    if found.is_empty() && problems.is_empty() {
        problems.push(Problem::new(format!(
            "{what}: no source writes it; the check would pass on nothing"
        )));
    }
    if found.len() > 1 {
        let values: Vec<String> = found
            .into_iter()
            .map(|(value, mut files)| {
                files.dedup();
                format!("{value} in {}", files.join(", "))
            })
            .collect();
        problems.push(Problem::new(format!(
            "{what} is not one value: {}",
            values.join("; ")
        )));
    }
    problems
}

/// `rust-toolchain.toml`'s channel.
static RUST_CHANNEL: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?m)^channel\s*=\s*"([^"]*)"\s*$"#));
/// A Cargo `rust-version`.
static RUST_VERSION: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?m)^rust-version\s*=\s*"([^"]*)"\s*$"#));
/// A Rust release with its patch: 1.98.1, never 1.98 or stable.
static EXACT_RUST: LazyLock<Regex> = LazyLock::new(|| regex(r"^\d+\.\d+\.\d+$"));
/// The Rust release of a `dhi.io/rust` base's tag, on a FROM line.
static RUST_IMAGE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?mi)^[ \t]*FROM[ \t]+(?:--\S+[ \t]+)*dhi\.io/rust:([0-9][\w.]*?)-"));
/// Any `dhi.io/rust` reference, with what follows the repository up to a word break: a FROM, a
/// `--from=`, a `--mount=from=`, an ARG default or any other text.
static RUST_IMAGE_REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?i)dhi\.io/rust([^\s"'\\,;)\]]*)"#));
/// The release of a reference's tag.
static RUST_IMAGE_RELEASE: LazyLock<Regex> = LazyLock::new(|| regex(r"^:(\d+\.\d+\.\d+)(?:-|$)"));
/// The release a build stage's musl standard libraries are downloaded for.
static RUST_VERSION_ARG: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^ARG RUST_VERSION=(\S+)\s*$"));

/// The exact release `rust-toolchain.toml` (the text `toolchain`, at `file`) pins.
///
/// # Errors
///
/// It names no channel, or one that is not an exact `X.Y.Z` release (`stable`, `1.98`, a
/// nightly).
pub fn rust_release(file: &str, toolchain: &str) -> Result<String, Problem> {
    match RUST_CHANNEL.captures(toolchain) {
        Some(channel) if EXACT_RUST.is_match(&channel[1]) => Ok(channel[1].to_owned()),
        Some(channel) => Err(Problem::at(
            file,
            format!("pins channel {:?}, not an exact X.Y.Z release", &channel[1]),
        )),
        None => Err(Problem::at(file, "names no channel")),
    }
}

/// The targets `rust-toolchain.toml` installs, empty when it names none or is not TOML.
#[must_use]
pub fn toolchain_targets(toolchain: &str) -> Vec<String> {
    toolchain
        .parse::<toml::Table>()
        .ok()
        .and_then(|table| {
            table
                .get("toolchain")
                .and_then(|section| section.get("targets"))
                .and_then(toml::Value::as_array)
                .map(|targets| {
                    targets
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// How a manifest's `rust-version` follows the exact release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RustVersion {
    /// The release itself, patch included (swarm-device-access, bitgauge).
    Exact,
    /// The release's line, `X.Y`, or a release of that line, so a consumer on an earlier patch
    /// release still builds (swarm-scheduler-exporter, rust-common).
    Line,
}

/// What is wrong with the `rust-version`s of `manifest` (the text of `file`) against `release`:
/// none at all, or one that is not the release (or, under [`RustVersion::Line`], its line).
#[must_use]
pub fn rust_version_problems(
    file: &str,
    manifest: &str,
    release: &str,
    policy: RustVersion,
) -> Vec<Problem> {
    let line = release.rsplit_once('.').map_or(release, |(line, _)| line);
    let versions: Vec<&str> = RUST_VERSION
        .captures_iter(manifest)
        .filter_map(|captures| captures.get(1).map(|version| version.as_str()))
        .collect();
    if versions.is_empty() {
        return vec![Problem::at(file, "sets no rust-version")];
    }
    versions
        .into_iter()
        .filter(|version| match policy {
            RustVersion::Exact => *version != release,
            RustVersion::Line => {
                *version != line
                    && !(EXACT_RUST.is_match(version)
                        && version.rsplit_once('.').is_some_and(|(of, _)| of == line))
            }
        })
        .map(|version| {
            Problem::at(
                file,
                match policy {
                    RustVersion::Exact => format!(
                        "rust-version is {version:?}, but rust-toolchain.toml pins {release}"
                    ),
                    RustVersion::Line => format!(
                        "rust-version is {version:?}, not the line {line} of rust-toolchain.toml's \
                         {release}"
                    ),
                },
            )
        })
        .collect()
}

/// What is wrong with an image's Rust against `release`: the `dhi.io/rust` tag of its build
/// stage's FROM and its `ARG RUST_VERSION` (the release its musl standard libraries are
/// downloaded for) are each written once, as an exact `X.Y.Z` release, and are the same release;
/// that release is `release` ([`RustVersion::Exact`]) or of its line ([`RustVersion::Line`],
/// awtrix-controller and swarm-rootless-docker, whose image may take a later patch); and every
/// other `dhi.io/rust` reference outside a comment line (a `COPY --from=`, a `RUN --mount=from=`,
/// an ARG default) has a tag of that same release, so no second Rust image builds a part.
#[must_use]
pub fn image_rust_problems(
    file: &str,
    dockerfile: &str,
    release: &str,
    policy: RustVersion,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut written = Vec::new();
    for (what, pattern) in [
        ("dhi.io/rust tag", &*RUST_IMAGE),
        ("ARG RUST_VERSION", &*RUST_VERSION_ARG),
    ] {
        let found: Vec<&str> = pattern
            .captures_iter(dockerfile)
            .filter_map(|captures| captures.get(1).map(|version| version.as_str()))
            .collect();
        match found.as_slice() {
            [version] if EXACT_RUST.is_match(version) => written.push(*version),
            [] => problems.push(Problem::at(file, format!("writes no {what}"))),
            [version] => problems.push(Problem::at(
                file,
                format!("the {what} is {version:?}, not an exact X.Y.Z release"),
            )),
            found => problems.push(Problem::at(
                file,
                format!(
                    "writes the {what} {} times ({found:?}), want once",
                    found.len()
                ),
            )),
        }
    }
    if let [tag, argument] = written.as_slice() {
        if tag != argument {
            problems.push(Problem::at(
                file,
                format!(
                    "the dhi.io/rust tag's release {tag} is not ARG RUST_VERSION's {argument}, so \
                     the musl standard libraries are another release's"
                ),
            ));
        }
        let line = |version: &str| {
            version
                .rsplit_once('.')
                .map_or(version, |(line, _)| line)
                .to_owned()
        };
        let matches = match policy {
            RustVersion::Exact => *tag == release,
            RustVersion::Line => line(tag) == line(release),
        };
        if !matches {
            problems.push(Problem::at(
                file,
                format!("builds with Rust {tag}, but rust-toolchain.toml pins {release}"),
            ));
        }
    }
    let tags: Vec<&str> = RUST_IMAGE
        .captures_iter(dockerfile)
        .filter_map(|captures| captures.get(1).map(|version| version.as_str()))
        .collect();
    if let [tag] = tags.as_slice()
        && EXACT_RUST.is_match(tag)
    {
        problems.extend(rust_reference_problems(file, dockerfile, tag));
    }
    problems
}

/// The `dhi.io/rust` references of `dockerfile`, outside comment lines, whose tag is not of the
/// build FROM's release `tag`; another repository under `dhi.io/rust` (`dhi.io/rust-<name>`) is
/// not one.
fn rust_reference_problems(file: &str, dockerfile: &str, tag: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    for line in dockerfile
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
    {
        for captures in RUST_IMAGE_REFERENCE.captures_iter(line) {
            let rest = &captures[1];
            if rest
                .chars()
                .next()
                .is_some_and(|next| next.is_ascii_alphanumeric() || "_.-/".contains(next))
            {
                continue;
            }
            let reference = &captures[0];
            match RUST_IMAGE_RELEASE.captures(rest) {
                Some(release) if &release[1] == tag => {}
                Some(_) => problems.push(Problem::at(
                    file,
                    format!(
                        "references {reference}, another release than the build stage's \
                         dhi.io/rust {tag}"
                    ),
                )),
                None => problems.push(Problem::at(
                    file,
                    format!("references {reference}, with no exact X.Y.Z release in its tag"),
                )),
            }
        }
    }
    problems
}

/// The actions that install a Rust toolchain, and whether each reads `rust-toolchain.toml` when
/// it names none.
pub const RUST_TOOLCHAIN_ACTIONS: &[(&str, bool)] = &[
    ("dtolnay/rust-toolchain", false),
    ("actions-rs/toolchain", false),
    ("actions-rust-lang/setup-rust-toolchain", true),
];

static COMMIT_REF: LazyLock<Regex> = LazyLock::new(|| regex(r"^[0-9a-f]{40}$"));

/// A toolchain a script names: `rustup` installing or selecting one, or `cargo +<toolchain>`.
/// Option flags before the toolchain are skipped; a flag's value is read as the toolchain, which
/// errs toward a false failure, never a silent pass.
static SCRIPT_TOOLCHAIN: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"\brustup\s+(?:toolchain\s+install|install|default|override\s+set)\s+",
        r"(?:-\S+\s+)*([A-Za-z0-9][\w.-]*)",
        r"|\bcargo\s+\+([A-Za-z0-9][\w.-]*)",
    ))
});

/// Where a toolchain action's toolchain may be read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolchainRef {
    /// Its `toolchain` input only: an action without one names no toolchain, whatever its `@`
    /// reference says. The actions are pinned by commit, and dtolnay's action pinned by commit
    /// takes the toolchain from the input alone (swarm-device-access).
    Input,
    /// Its `toolchain` input, or else its `@` reference when that is not a commit (dtolnay's
    /// `@1.98.1` form; bitgauge).
    InputOrRef,
}

/// Every Rust toolchain the CI documents (path and parsed YAML) name, and a problem for every
/// toolchain action that names none and does not read `rust-toolchain.toml`, or when no step sets
/// up Rust at all. An action's toolchain is read as `refs` says; a script's is a
/// `RUSTUP_TOOLCHAIN` variable, or a `rustup` or `cargo +` command in any string.
#[must_use]
pub fn ci_rust_toolchains(
    documents: &[(String, Value)],
    refs: ToolchainRef,
) -> (Vec<PinValue>, Vec<Problem>) {
    let steps: Vec<ActionStep> = documents
        .iter()
        .flat_map(|(file, document)| action_steps(file, document))
        .collect();
    let (mut values, mut problems) = action_rust_toolchains(&steps, refs);
    if !steps.iter().any(|step| {
        RUST_TOOLCHAIN_ACTIONS
            .iter()
            .any(|(action, _)| *action == step.action)
    }) {
        problems.push(Problem::new(
            "no CI step sets up a Rust toolchain; the check would see nothing",
        ));
    }
    for (file, document) in documents {
        values.extend(script_rust_toolchains(file, document));
    }
    (values, problems)
}

fn action_rust_toolchains(
    steps: &[ActionStep],
    refs: ToolchainRef,
) -> (Vec<PinValue>, Vec<Problem>) {
    let mut values = Vec::new();
    let mut problems = Vec::new();
    for step in steps {
        let Some(&(_, reads_file)) = RUST_TOOLCHAIN_ACTIONS
            .iter()
            .find(|(action, _)| *action == step.action)
        else {
            continue;
        };
        if let Some(toolchain) = step.with.get("toolchain") {
            values.push(PinValue::new(toolchain, &step.file));
        } else if refs == ToolchainRef::InputOrRef
            && !step.reference.is_empty()
            && !COMMIT_REF.is_match(&step.reference)
        {
            values.push(PinValue::new(&step.reference, &step.file));
        } else if !reads_file {
            problems.push(Problem::at(
                &step.file,
                format!(
                    "{} names no toolchain, and does not read rust-toolchain.toml",
                    step.action
                ),
            ));
        }
    }
    (values, problems)
}

fn script_rust_toolchains(file: &str, document: &Value) -> Vec<PinValue> {
    let mut values = Vec::new();
    let mut pending = vec![document];
    while let Some(node) = pending.pop() {
        match node {
            Value::Object(map) => {
                if let Some(toolchain) = map.get("RUSTUP_TOOLCHAIN") {
                    values.push(PinValue::new(&scalar_text(toolchain), file));
                }
                pending.extend(map.values());
            }
            Value::Array(items) => pending.extend(items),
            Value::String(text) => {
                for captures in SCRIPT_TOOLCHAIN.captures_iter(text) {
                    if let Some(toolchain) = captures.get(1).or_else(|| captures.get(2)) {
                        values.push(PinValue::new(toolchain.as_str(), file));
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    values
}

/// What is wrong with the Rust toolchains CI names ([`ci_rust_toolchains`], read as `refs` says)
/// against `release`: each must be exactly the release, never a channel or a release without its
/// patch.
#[must_use]
pub fn ci_toolchain_problems(
    documents: &[(String, Value)],
    release: &str,
    refs: ToolchainRef,
) -> Vec<Problem> {
    let (values, mut problems) = ci_rust_toolchains(documents, refs);
    problems.extend(
        values
            .into_iter()
            .filter(|value| value.value != release)
            .map(|value| {
                Problem::at(
                    value.file,
                    format!(
                        "installs toolchain {:?}; it must be {release}, the release of \
                         rust-toolchain.toml",
                        value.value
                    ),
                )
            }),
    );
    problems
}

/// The release fragment's musl standard library pins.
static RUST_STD_VERSION: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^RUST_RELEASE_RUST_STD_VERSION\s*:?=\s*(\S+)\s*$"));
/// A rust-std digest line, of any value, so a malformed digest is seen rather than skipped.
static RUST_STD_SHA256: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^RUST_RELEASE_RUST_STD_SHA256_(\S+)\s*:?=\s*(\S*)\s*$"));
/// The release platforms, as `<docker arch>:<Rust target>` pairs.
static RELEASE_PLATFORMS: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^RUST_RELEASE_PLATFORMS\s*:?=\s*(.*)$"));
static SHA256: LazyLock<Regex> = LazyLock::new(|| regex(r"^[0-9a-f]{64}$"));

/// What is wrong with a release fragment's targets and musl standard libraries (`fragment`, the
/// text of `file`, such as make-common's `.mk/rust-release.mk`) against the exact release and the
/// targets of `rust-toolchain.toml`: the rust-std release is the toolchain's, each target has one
/// well-formed digest and no other target has one, and the release platforms build exactly those
/// targets, in that order. Where make reads the last assignment of a variable, so does this.
#[must_use]
pub fn rust_std_problems(
    file: &str,
    fragment: &str,
    release: &str,
    targets: &[String],
) -> Vec<Problem> {
    let mut problems = Vec::new();
    if targets.is_empty() {
        problems.push(Problem::new(
            "rust-toolchain.toml installs no target; the check would see nothing",
        ));
    }
    match RUST_STD_VERSION.captures_iter(fragment).last() {
        Some(version) if version[1] == *release => {}
        Some(version) => problems.push(Problem::at(
            file,
            format!(
                "pins rust-std {}, but rust-toolchain.toml says {release}",
                &version[1]
            ),
        )),
        None => problems.push(Problem::at(file, "writes no RUST_RELEASE_RUST_STD_VERSION")),
    }
    let mut digests: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for captures in RUST_STD_SHA256.captures_iter(fragment) {
        if let (Some(target), Some(digest)) = (captures.get(1), captures.get(2)) {
            digests
                .entry(target.as_str())
                .or_default()
                .push(digest.as_str());
        }
    }
    for target in targets {
        match digests.get(target.as_str()).map(Vec::as_slice) {
            Some([digest]) if SHA256.is_match(digest) => {}
            Some([digest]) => problems.push(Problem::at(
                file,
                format!("the rust-std digest of {target} is {digest:?}, not a sha256"),
            )),
            Some(written) => problems.push(Problem::at(
                file,
                format!(
                    "the rust-std digest of {target} is written {} times, want once",
                    written.len()
                ),
            )),
            None => problems.push(Problem::at(
                file,
                format!("{target} has no rust-std digest"),
            )),
        }
    }
    for target in digests.keys() {
        if !targets.iter().any(|wanted| wanted == target) {
            problems.push(Problem::at(
                file,
                format!(
                    "a rust-std digest names {target}, which rust-toolchain.toml does not install"
                ),
            ));
        }
    }
    let platforms: Vec<&str> = RELEASE_PLATFORMS
        .captures_iter(fragment)
        .last()
        .and_then(|captures| captures.get(1))
        .map(|platforms| {
            platforms
                .as_str()
                .split_whitespace()
                .filter_map(|platform| platform.split_once(':').map(|(_, target)| target))
                .collect()
        })
        .unwrap_or_default();
    if platforms != targets.iter().map(String::as_str).collect::<Vec<_>>() {
        problems.push(Problem::at(
            file,
            format!(
                "the release platforms build {platforms:?}, but rust-toolchain.toml installs \
                 {targets:?}"
            ),
        ));
    }
    problems
}

/// One archive a Dockerfile's build stage downloads and checks: the shell variable that names it
/// in a `case` arm (`zig_arch`), the one that holds its digest (`zig_sha256`), the prefix of the
/// release fragment's variable for it, whose suffix is the name (`RUST_RELEASE_ZIG_SHA256_`), and
/// the arms the stage must have: each platform the `case` switches on, with the archive's name
/// for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseArchive<'pins> {
    /// The variable naming the archive: `zig_arch=x86_64;`.
    pub key: &'pins str,
    /// The variable holding its digest: `zig_sha256=<sha256>`.
    pub sha: &'pins str,
    /// The fragment's variable prefix: `RUST_RELEASE_ZIG_SHA256_<name> := <sha256>`.
    pub prefix: &'pins str,
    /// The required arms, as (Docker architecture, name): `("amd64", "x86_64")`.
    pub arms: &'pins [(&'pins str, &'pins str)],
}

/// The release toolchain pins a Dockerfile repeats from the release fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleasePins<'pins> {
    /// Each version, as (the Dockerfile's `ARG`, the fragment's variable).
    pub versions: &'pins [(&'pins str, &'pins str)],
    /// Each archive the build stage checks.
    pub archives: &'pins [ReleaseArchive<'pins>],
}

/// The pins of make-common's `.mk/rust-release.mk` and the consumers' build stages: zig,
/// cargo-zigbuild and the musl rust-std, each version, and each archive's digest for amd64 and
/// arm64.
pub const RUST_RELEASE_PINS: ReleasePins<'static> = ReleasePins {
    versions: &[
        ("ZIG_VERSION", "RUST_RELEASE_ZIG_VERSION"),
        (
            "CARGO_ZIGBUILD_VERSION",
            "RUST_RELEASE_CARGO_ZIGBUILD_VERSION",
        ),
        ("RUST_VERSION", "RUST_RELEASE_RUST_STD_VERSION"),
    ],
    archives: &[
        ReleaseArchive {
            key: "zig_arch",
            sha: "zig_sha256",
            prefix: "RUST_RELEASE_ZIG_SHA256_",
            arms: &[("amd64", "x86_64"), ("arm64", "aarch64")],
        },
        ReleaseArchive {
            key: "zigbuild_asset",
            sha: "zigbuild_sha256",
            prefix: "RUST_RELEASE_CARGO_ZIGBUILD_SHA256_",
            arms: &[
                ("amd64", "x86_64-unknown-linux-musl"),
                ("arm64", "aarch64-unknown-linux-gnu"),
            ],
        },
        ReleaseArchive {
            key: "triple",
            sha: "std_sha256",
            prefix: "RUST_RELEASE_RUST_STD_SHA256_",
            arms: &[
                ("amd64", "x86_64-unknown-linux-musl"),
                ("arm64", "aarch64-unknown-linux-musl"),
            ],
        },
    ],
};

static ANY_SHA256: LazyLock<Regex> = LazyLock::new(|| regex(r"\b[0-9a-f]{64}\b"));
/// An image or frontend digest: its pin is the Dockerfile's own, not a release archive's.
static IMAGE_DIGEST: LazyLock<Regex> = LazyLock::new(|| regex(r"@sha256:[0-9a-f]{64}\b"));

/// The value make gives `<variable>` in `text`, assignments read in order: `=`, `:=` and `::=`
/// set it, `?=` only when it is unset, `+=` appends with a space, and `!=` (a shell command's
/// output) is kept as `!=<command>`, which matches no pin; an `export` or `override` prefix is
/// read past. `None` when nothing assigns it.
#[must_use]
pub fn make_value(text: &str, variable: &str) -> Option<String> {
    let assignment = pattern(&format!(
        r"(?m)^(?:(?:export|override)[ \t]+)*{}[ \t]*(::|:|\?|\+|!)?=[ \t]*(.*?)[ \t]*$",
        regex::escape(variable)
    ));
    let mut value: Option<String> = None;
    for captures in assignment.captures_iter(text) {
        let assigned = captures[2].to_owned();
        value = match (captures.get(1).map(|flavour| flavour.as_str()), value) {
            (Some("?"), Some(current)) => Some(current),
            (Some("+"), Some(current)) => Some(format!("{current} {assigned}")),
            (Some("!"), _) => Some(format!("!={assigned}")),
            (_, _) => Some(assigned),
        };
    }
    value
}

/// One `(platform, name, digest)` arm.
type Arm = (String, String, String);

/// The arms of one archive against the fragment: how many well-formed digests the Dockerfile's
/// arms carry, and the problems ([`release_pin_problems`]).
fn archive_problems(
    dockerfile_file: &str,
    dockerfile: &str,
    fragment_file: &str,
    fragment: &str,
    archive: &ReleaseArchive<'_>,
) -> (usize, Vec<Problem>) {
    let mut problems = Vec::new();
    if archive.arms.is_empty() {
        problems.push(Problem::new(format!(
            "{}: no arm is required; the check would pass on nothing",
            archive.sha
        )));
        return (0, problems);
    }
    let case = case_arm::arms(dockerfile, archive.key, archive.sha);
    let found: BTreeSet<Arm> = case.labelled();
    let mut recorded: BTreeMap<String, String> = BTreeMap::new();
    for captures in pattern(&format!(
        r"(?m)^{}([\w.-]+)[ \t]*:?=[ \t]*(\S*)[ \t]*$",
        regex::escape(archive.prefix)
    ))
    .captures_iter(fragment)
    {
        recorded.insert(captures[1].to_owned(), captures[2].to_owned());
    }
    let mut want = BTreeSet::new();
    for (platform, name) in archive.arms {
        match recorded.get(*name) {
            Some(digest) if SHA256.is_match(digest) => {
                want.insert(((*platform).to_owned(), (*name).to_owned(), digest.clone()));
            }
            Some(digest) => problems.push(Problem::at(
                fragment_file,
                format!("{}{name} is {digest:?}, not a sha256", archive.prefix),
            )),
            None => problems.push(Problem::at(
                fragment_file,
                format!("sets no {}{name}", archive.prefix),
            )),
        }
    }
    for name in recorded.keys() {
        if !archive.arms.iter().any(|(_, wanted)| wanted == name) {
            problems.push(Problem::at(
                fragment_file,
                format!("sets {}{name}, which no required arm names", archive.prefix),
            ));
        }
    }
    if found == want {
        problems.extend(
            case.problems(archive.key, archive.sha, &want)
                .into_iter()
                .map(|problem| Problem::at(dockerfile_file, problem)),
        );
    } else {
        problems.push(Problem::at(
            dockerfile_file,
            format!(
                "{} is chosen by the arms {found:?}, want exactly {want:?} from {fragment_file}",
                archive.sha
            ),
        ));
    }
    // Each arm once, however many labels its pattern lists.
    let paired = case
        .arms
        .iter()
        .filter(|arm| SHA256.is_match(&arm.sha))
        .count();
    (paired, problems)
}

/// What is wrong with the release pins `dockerfile` (the text of `dockerfile_file`) repeats from
/// `fragment` (the text of `fragment_file`): each version is the fragment's (make's last
/// assignment); each archive's `case` arms are exactly its required arms, each platform with its
/// name and the fragment's digest for that name, so a swapped, dropped or extra arm fails; the
/// fragment holds a well-formed digest for each required name and none for another; and every
/// other sha256 in the Dockerfile is an image or frontend digest, so no download is checked
/// against a digest the record does not hold. Pins with no version and no archive are a problem.
#[must_use]
pub fn release_pin_problems(
    dockerfile_file: &str,
    dockerfile: &str,
    fragment_file: &str,
    fragment: &str,
    pins: &ReleasePins<'_>,
) -> Vec<Problem> {
    if pins.versions.is_empty() && pins.archives.is_empty() {
        return vec![Problem::new(
            "the release pins name no version and no archive; the check would pass on nothing",
        )];
    }
    let mut problems = Vec::new();
    for (argument, variable) in pins.versions {
        let in_docker: Vec<&str> =
            pattern(&format!(r"(?m)^ARG {}=(\S+)\s*$", regex::escape(argument)))
                .captures_iter(dockerfile)
                .filter_map(|captures| captures.get(1).map(|value| value.as_str()))
                .collect();
        let in_fragment = make_value(fragment, variable);
        match (in_docker.as_slice(), in_fragment) {
            ([docker], Some(record)) if *docker == record => {}
            (_, None) => problems.push(Problem::at(
                fragment_file,
                format!("does not set {variable}"),
            )),
            (found, Some(record)) => problems.push(Problem::at(
                dockerfile_file,
                format!(
                    "writes ARG {argument} as {found:?}, want once as {fragment_file}'s {record}"
                ),
            )),
        }
    }
    let mut paired = 0;
    for archive in pins.archives {
        let (found, archive_problems) = archive_problems(
            dockerfile_file,
            dockerfile,
            fragment_file,
            fragment,
            archive,
        );
        paired += found;
        problems.extend(archive_problems);
    }
    let digests = IMAGE_DIGEST.find_iter(dockerfile).count();
    let hashes = ANY_SHA256.find_iter(dockerfile).count();
    if hashes != paired + digests {
        problems.push(Problem::at(
            dockerfile_file,
            format!(
                "writes {hashes} sha256s: {paired} release archives and {digests} image digests \
                 account for the rest, so one is not {fragment_file}'s"
            ),
        ));
    }
    problems
}

/// Where the zig and cargo-zigbuild pins are recorded, against which the setup-zigbuild action is
/// held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZigRecord<'record> {
    /// A release fragment, `RUST_RELEASE_ZIG_VERSION := ...` and the `x86_64` digests
    /// (swarm-device-access, bitgauge).
    Fragment {
        /// The fragment, relative to the root.
        file: &'record str,
        /// Its text.
        content: &'record str,
    },
    /// A Dockerfile's build stage, `ARG ZIG_VERSION=...` and the `amd64` case arms
    /// (swarm-scheduler-exporter).
    Dockerfile {
        /// The Dockerfile, relative to the root.
        file: &'record str,
        /// Its text.
        content: &'record str,
    },
}

/// What is wrong between the setup-zigbuild action (`action`, the text of `action_file`) and the
/// record: zig and cargo-zigbuild are one version each, from the same `x86_64` archives (both
/// download those), so a bump that moves one copy fails here rather than in the first build that
/// runs it.
#[must_use]
pub fn zigbuild_action_problems(
    action_file: &str,
    action: &str,
    record: ZigRecord<'_>,
) -> Vec<Problem> {
    let action_patterns = [
        r#"(?m)^\s*ZIG_VERSION:\s*"?(\d+\.\d+\.\d+)"?\s*(?:#.*)?$"#,
        r"(?m)^\s*ZIG_SHA256:\s*([0-9a-f]{64})\s*(?:#.*)?$",
        r#"(?m)^\s*CARGO_ZIGBUILD_VERSION:\s*"?(\d+\.\d+\.\d+)"?\s*(?:#.*)?$"#,
        r"(?m)^\s*CARGO_ZIGBUILD_SHA256:\s*([0-9a-f]{64})\s*(?:#.*)?$",
    ];
    let (file, content, record_patterns) = match record {
        ZigRecord::Fragment { file, content } => (
            file,
            content,
            [
                r"(?m)^RUST_RELEASE_ZIG_VERSION\s*:?=\s*(\S+)\s*$",
                r"(?m)^RUST_RELEASE_ZIG_SHA256_x86_64\s*:?=\s*([0-9a-f]{64})\s*$",
                r"(?m)^RUST_RELEASE_CARGO_ZIGBUILD_VERSION\s*:?=\s*(\S+)\s*$",
                r"(?m)^RUST_RELEASE_CARGO_ZIGBUILD_SHA256_x86_64-unknown-linux-musl\s*:?=\s*([0-9a-f]{64})\s*$",
            ],
        ),
        ZigRecord::Dockerfile { file, content } => (
            file,
            content,
            [
                r"(?m)^ARG ZIG_VERSION=(\S+)\s*$",
                r"amd64\) zig_arch=x86_64; zig_sha256=([0-9a-f]{64}) ;;",
                r"(?m)^ARG CARGO_ZIGBUILD_VERSION=(\S+)\s*$",
                r"amd64\) zigbuild_asset=x86_64-unknown-linux-musl; zigbuild_sha256=([0-9a-f]{64}) ;;",
            ],
        ),
    };
    [
        "zig",
        "zig x86_64 sha256",
        "cargo-zigbuild",
        "cargo-zigbuild x86_64 sha256",
    ]
    .into_iter()
    .zip(action_patterns.into_iter().zip(record_patterns))
    .flat_map(|(what, (action_pattern, record_pattern))| {
        one_value(
            what,
            &[
                PinSource::new(action_file, action, action_pattern),
                PinSource::new(file, content, record_pattern),
            ],
            &[],
        )
    })
    .collect()
}

/// What is wrong between the ShellCheck the shellcheck-py hook wraps (its rev adds a fourth
/// component for the wrapper, so only the first three count) in `hooks` (the text of
/// `hooks_file`) and the one the setup-shellcheck action installs (`action`, the text of
/// `action_file`), which tools run from `PATH` in CI.
#[must_use]
pub fn shellcheck_problems(
    hooks_file: &str,
    hooks: &str,
    action_file: &str,
    action: &str,
) -> Vec<Problem> {
    one_value(
        "ShellCheck",
        &[
            PinSource::new(
                hooks_file,
                hooks,
                r"repo: https://github\.com/shellcheck-py/shellcheck-py\s+rev:\s*(v\d+\.\d+\.\d+)",
            ),
            PinSource::new(
                action_file,
                action,
                r#"SHELLCHECK_VERSION:\s*["']?(v[\d.]+)"#,
            ),
        ],
        &[],
    )
}

/// The action that installs Trivy.
pub const SETUP_TRIVY: &str = "aquasecurity/setup-trivy";
static TRIVY_VERSION: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?m)^\s*TRIVY_VERSION:\s*["']?([^"'\s#]*)["']?\s*(?:#.*)?$"#));
static EXACT_TRIVY: LazyLock<Regex> = LazyLock::new(|| regex(r"^v\d+\.\d+\.\d+$"));

/// What is wrong with the Trivy the scanning `workflows` (path and text) install: each writes
/// `TRIVY_VERSION` once, as an exact `vX.Y.Z`, has a setup-trivy step, and every such step
/// installs exactly `${{ env.TRIVY_VERSION }}`; and all of them write one version, so a finding
/// means the same in every scan. No workflow at all is a problem. A trailing comment after the
/// version is read past.
#[must_use]
pub fn trivy_problems(workflows: &[(String, String)]) -> Vec<Problem> {
    if workflows.is_empty() {
        return vec![Problem::new(
            "no scanning workflow is given; the Trivy check would pass on nothing",
        )];
    }
    let mut problems = Vec::new();
    let mut extra = Vec::new();
    for (file, content) in workflows {
        let pins: Vec<&str> = TRIVY_VERSION
            .captures_iter(content)
            .filter_map(|captures| captures.get(1).map(|version| version.as_str()))
            .collect();
        match pins.as_slice() {
            [version] if EXACT_TRIVY.is_match(version) => extra.push(PinValue::new(version, file)),
            [version] => problems.push(Problem::at(
                file,
                format!("pins Trivy {version:?}, not an exact vX.Y.Z release"),
            )),
            pins => problems.push(Problem::at(
                file,
                format!("writes TRIVY_VERSION {} times, want once", pins.len()),
            )),
        }
        let document = match crate::yaml::parse(file, content) {
            Ok(document) => document,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        let steps: Vec<ActionStep> = action_steps(file, &document)
            .into_iter()
            .filter(|step| step.action == SETUP_TRIVY)
            .collect();
        if steps.is_empty() {
            problems.push(Problem::at(file, format!("has no {SETUP_TRIVY} step")));
        }
        for step in steps {
            let version = step.with.get("version").map_or("", String::as_str);
            if version != "${{ env.TRIVY_VERSION }}" {
                problems.push(Problem::at(
                    file,
                    format!(
                        "{SETUP_TRIVY} installs version {version:?}, want ${{{{ env.TRIVY_VERSION }}}}"
                    ),
                ));
            }
        }
    }
    if !extra.is_empty() {
        problems.extend(one_value("Trivy", &[], &extra));
    }
    problems
}

#[cfg(test)]
mod tests;
