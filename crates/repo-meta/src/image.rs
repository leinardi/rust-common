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

//! The shipped image's invariants, read from the files with [`crate::dockerfile`] so no tool is
//! needed: one Dockerfile, a frontend pinned by digest, the stages from bases pinned as the
//! project's [`BasePolicy`] requires, the build stage (the build platform, the exact build image
//! or its repository, what it must do, and each download at its pinned release, chosen by its
//! platform argument and checked against exactly its pinned digests), the version stamps from the
//! Dockerfile and make alike, the runtime posture and its labels, the platforms the release
//! workflow declares, and the labels `droast.toml` requires.
//!
//! Each rule takes its project's values: the stage names, the base policy, the images, the
//! instruction texts, the downloads, the stamp prefix, the platforms and the labels.
//!
//! What stays in a consumer's own meta-tests: swarm-rootless-docker's exact dind image, its rules
//! that the last USER is the unprivileged one, that the user rewrite is the stage's one RUN and
//! that nothing is installed with apk, and its `base.name` label; awtrix-controller's and
//! swarm-rootless-docker's check that the release workflow supplies every label argument
//! (`label_arguments_are_supplied`); and every check that runs a tool, such as droast.
//!
//! Extracted from swarm-device-access/meta/tests/dockerfiles.rs at 8b4ab3a3e417 (the rules as
//! functions of the contents, `build_stage_problems`, `stamp_problems`, `runtime_problems`,
//! `platform_problems`, `label_policy_problems`) with
//! swarm-scheduler-exporter/meta/tests/dockerfiles.rs at c9e79afa8810 (each stamp from its make
//! variable, and the prefix as a whole line of the Makefile) and
//! `bitgauge/meta/tests/dockerfiles.rs` and `bitgauge/meta/tests/dockerfiles/images.rs` and
//! `release.rs` at 600fc3905200 (the Dockerfile names and their listing, every base from dhi.io,
//! the exact build image, each download's rules checked in the RUN that fetches it, the stamps in
//! the RUN that builds, and the release recipe that carries them).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use toml::Table;

use crate::dockerfile::{self, Stage};
use crate::{Problem, Repo, pattern, regex};

/// Whether a file name is a Dockerfile: `Dockerfile`, `<name>.Dockerfile` or `Dockerfile.<name>`,
/// but not the `Dockerfile.<name>.dockerignore` `BuildKit` reads beside one.
fn is_dockerfile(name: &str) -> bool {
    name == "Dockerfile"
        || name.ends_with(".Dockerfile")
        || (name.starts_with("Dockerfile.") && !name.ends_with(".dockerignore"))
}

/// Every Dockerfile under `repo` that is tracked or new ([`Repo::tracked_or_new`]), sorted: git
/// keeps the ignored trees (`target/`, `node_modules/`, `dist/`) out without naming them, and a
/// hidden directory is searched like any other.
///
/// # Panics
///
/// git cannot list the files.
#[must_use]
pub fn dockerfiles(repo: &Repo) -> Vec<String> {
    repo.tracked_or_new(&["*Dockerfile*"])
        .into_iter()
        .filter(|path| is_dockerfile_path(path))
        .collect()
}

/// What is wrong unless `found` (from [`dockerfiles`]) is exactly `shipped`: a second Dockerfile
/// would look shipped to every reader and be built by nothing.
#[must_use]
pub fn only_dockerfile_problems(found: &[String], shipped: &str) -> Vec<Problem> {
    if found == [shipped] {
        Vec::new()
    } else {
        vec![Problem::new(format!(
            "the repository holds the Dockerfiles {found:?}; want only {shipped}"
        ))]
    }
}

/// What is wrong with the syntax directive of `content` (the text of `file`): it must name
/// `docker/dockerfile`, by tag and digest, so the frontend cannot be moved under a build. With
/// `required` false a file may have none (`BuildKit`'s own frontend), but one it has is held to
/// the same rule.
#[must_use]
pub fn syntax_problems(file: &str, content: &str, required: bool) -> Vec<Problem> {
    match dockerfile::syntax(content) {
        None if required => vec![Problem::at(
            file,
            "has no syntax parser directive on its first lines",
        )],
        Some(syntax) if dockerfile::repository(&syntax) != "docker/dockerfile" => {
            vec![Problem::at(
                file,
                format!("the frontend {syntax:?} is not docker/dockerfile"),
            )]
        }
        Some(syntax) if !dockerfile::digest_pinned(&syntax) => vec![Problem::at(
            file,
            format!("the frontend {syntax:?} is not pinned by tag and digest"),
        )],
        None | Some(_) => Vec::new(),
    }
}

/// How a project pins its stages' bases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BasePolicy {
    /// A dhi.io base by an explicit tag alone, any other by tag and digest
    /// ([`dockerfile::base_pinned`]; swarm-scheduler-exporter, swarm-device-access,
    /// awtrix-controller).
    DhiTagElseDigest,
    /// Every base from dhi.io, by an explicit tag alone (bitgauge).
    DhiOnly,
    /// Every base by an explicit tag alone, from any registry, such as
    /// `docker:29-dind-rootless` (swarm-rootless-docker, whose base moves with its tag on
    /// purpose).
    TagOnly,
}

impl BasePolicy {
    fn allows(self, image: &str) -> bool {
        match self {
            Self::DhiTagElseDigest => dockerfile::base_pinned(image),
            Self::DhiOnly => dockerfile::from_dhi(image) && dockerfile::tag_pinned(image),
            Self::TagOnly => dockerfile::tag_pinned(image),
        }
    }

    fn rule(self) -> &'static str {
        match self {
            Self::DhiTagElseDigest => {
                "dhi.io by an explicit tag alone, any other by tag and digest"
            }
            Self::DhiOnly => "a literal dhi.io reference by an explicit tag alone",
            Self::TagOnly => "a literal reference by an explicit tag alone",
        }
    }
}

/// What is wrong with the stages of `content` (the text of `file`): they must be exactly `names`,
/// in order, each from a base `policy` allows; and `names` must name one, or the check would pass
/// on nothing.
#[must_use]
pub fn stage_problems(
    file: &str,
    content: &str,
    names: &[&str],
    policy: BasePolicy,
) -> Vec<Problem> {
    if names.is_empty() {
        return vec![Problem::at(
            file,
            "no stage is wanted; the check would pass on nothing",
        )];
    }
    let stages = match dockerfile::parse(content) {
        Ok(stages) => stages,
        Err(error) => return vec![Problem::at(file, error.to_string())],
    };
    let mut problems = Vec::new();
    let found: Vec<&str> = stages.iter().map(|stage| stage.name.as_str()).collect();
    if found != names {
        problems.push(Problem::at(
            file,
            format!("has the stages {found:?}, want {names:?}"),
        ));
    }
    for stage in &stages {
        if !policy.allows(&stage.image) {
            problems.push(Problem::at(
                file,
                format!(
                    "stage {:?} is FROM {:?}, which is not pinned as the project requires ({})",
                    stage.name,
                    stage.image,
                    policy.rule()
                ),
            ));
        }
    }
    problems
}

/// The base a build stage must start from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildBase<'rules> {
    /// Any reference of this repository, such as `dhi.io/node`.
    Repository(&'rules str),
    /// Exactly this reference, such as `dhi.io/rust:1.98.1-debian13-dev`: the toolchain's
    /// release and the debian variant, whose toolchain takes the upstream musl standard
    /// libraries where the alpine variant's is Alpine's own build.
    Exact(&'rules str),
}

/// One archive a build stage downloads: the URL its RUN fetches (naming the release through a
/// build argument), the platform argument its `case` switches on, and the variables each arm
/// sets, with the release and the `(platform, asset, sha256)` arms it must carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download<'rules> {
    /// The tool, named in a problem.
    pub tool: &'rules str,
    /// The URL exactly as the RUN writes it, such as
    /// `https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz`.
    pub url: &'rules str,
    /// The platform argument the `case` switches on: `BUILDARCH` for a tool that runs on the
    /// build platform, `TARGETARCH` for what the binary is built for.
    pub platform_arg: &'rules str,
    /// The build argument naming the release, such as `ZIG_VERSION`.
    pub version_arg: &'rules str,
    /// The release it must have.
    pub version: String,
    /// The variable an arm names the asset with, such as `zig_arch`.
    pub asset_var: &'rules str,
    /// The variable an arm holds the asset's sha256 in, such as `zig_sha256`.
    pub sha_var: &'rules str,
    /// The arms, exactly: `(platform, asset, sha256)`, such as `("amd64", "x86_64", "1cbe…")`.
    pub arms: Vec<(String, String, String)>,
}

/// A build stage's rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildStage<'rules> {
    /// The `--platform` it must run on, such as `$BUILDPLATFORM`.
    pub platform: &'rules str,
    /// Its base.
    pub base: BuildBase<'rules>,
    /// Texts its joined instructions must contain, such as the cargo command with `--locked` and
    /// the copy of the binary.
    pub required: &'rules [&'rules str],
    /// Every archive it downloads; a RUN that downloads anything else is a problem, so a stage
    /// that downloads nothing passes with none.
    pub downloads: &'rules [Download<'rules>],
}

/// A download command in a RUN: curl or wget where a shell command starts (the start of the RUN,
/// or after `&&`, `||`, `;`, `|`, `(`, `then`, `do`, past any `NAME=value` assignments), whatever
/// it fetches, a literal URL or a variable; `apt-get install curl` runs no curl.
static FETCH: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?:^|&&|\|\||;|\||\(|\bthen\b|\bdo\b)\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*(?:curl|wget)\b",
    )
});
/// A curl or wget run anywhere in a RUN, command start or not (`if`, a `case` arm, `env`,
/// `command`, a path prefix such as `/usr/bin/`, `{ }`, backticks, `sh -c`, `xargs`): the word,
/// not part of a longer name, followed by a flag, a variable, a quoted argument or a URL. A package
/// name (`apt-get install -y curl wget`) is followed by none of these.
static INVOCATION: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?:^|[^\w.-])(?:curl|wget)\s+(?:-|\$|["']|https?://|ftp://)"#));
/// The consumers' own fetch marker: their per-stage rule counts every `curl -fsSL`.
const CURL_FETCH: &str = "curl -fsSL";
/// The check every fetch must have in its RUN.
const SHA256_CHECK: &str = "| sha256sum -c -";

/// How many files a RUN fetches: the largest of the command-start count, the invocation count and
/// the consumers' `curl -fsSL` count, so no reading can hide a fetch another sees. The invocation
/// count sees every fetch the other two do in the consumers' shapes; the other two stay as floors.
fn fetch_count(run: &str) -> usize {
    FETCH
        .find_iter(run)
        .count()
        .max(INVOCATION.find_iter(run).count())
        .max(run.matches(CURL_FETCH).count())
}

/// What is wrong with the fetches of one RUN: every fetch must be one a download rule names in
/// this RUN (a RUN holding one listed URL cannot fetch a second file beside it), and the RUN must
/// check as many files with `sha256sum -c -` as it fetches, the consumers' per-stage rule held
/// per RUN.
fn run_fetch_problems(run: &str, downloads: &[Download<'_>]) -> Vec<Problem> {
    let fetches = fetch_count(run);
    if fetches == 0 {
        return Vec::new();
    }
    let mut problems = Vec::new();
    let credited = downloads
        .iter()
        .filter(|download| run.contains(download.url))
        .count();
    if fetches != credited {
        problems.push(Problem::new(format!(
            "the build stage's RUN {run} fetches {fetches} files, and its download rules name \
             {credited}"
        )));
    }
    let checks = run.matches(SHA256_CHECK).count();
    if fetches != checks {
        problems.push(Problem::new(format!(
            "the build stage's RUN {run} fetches {fetches} files and checks {checks} with \
             sha256sum -c -"
        )));
    }
    problems
}

/// Whether an ADD fetches: one of its sources (the arguments before the destination, past the
/// flags) is a URL or a variable, which may hold one.
fn add_fetches(args: &str) -> bool {
    let fields: Vec<&str> = args
        .split_whitespace()
        .filter(|field| !field.starts_with("--"))
        .collect();
    fields.split_last().is_some_and(|(_, sources)| {
        sources
            .iter()
            .any(|source| source.contains("://") || source.contains('$'))
    })
}

/// The build arguments a stage declares, each with its default, quotes removed; `None` for one
/// declared with no default.
fn stage_args(stage: &Stage) -> BTreeMap<&str, Option<&str>> {
    stage
        .instructions
        .iter()
        .filter(|instruction| instruction.keyword == "ARG")
        .map(|instruction| match instruction.args.split_once('=') {
            Some((name, default)) => (name, Some(default.trim_matches('"'))),
            None => (instruction.args.as_str(), None),
        })
        .collect()
}

/// The RUN instructions of `stage` that contain `needle`.
fn runs_with<'stage>(stage: &'stage Stage, needle: &str) -> Vec<&'stage str> {
    stage
        .instructions
        .iter()
        .filter(|instruction| instruction.keyword == "RUN" && instruction.args.contains(needle))
        .map(|instruction| instruction.args.as_str())
        .collect()
}

/// What is wrong with a build stage against `rules`: its platform, its base, the required texts,
/// each download ([`Download`]) checked in the one RUN that fetches its URL, any RUN whose fetches
/// (curl or wget, of a literal URL or a variable) are not each a download rule's and each checked
/// with `sha256sum -c -` there, and any ADD of a URL or a variable.
#[must_use]
pub fn build_stage_problems(build: &Stage, rules: &BuildStage<'_>) -> Vec<Problem> {
    let mut problems = Vec::new();
    if build.platform != rules.platform {
        problems.push(Problem::new(format!(
            "the build stage runs on --platform={:?}, want {}",
            build.platform, rules.platform
        )));
    }
    match rules.base {
        BuildBase::Repository(repository) if dockerfile::repository(&build.image) != repository => {
            problems.push(Problem::new(format!(
                "the build stage is FROM {:?}, want {repository}",
                build.image
            )));
        }
        BuildBase::Exact(image) if build.image != image => problems.push(Problem::new(format!(
            "the build stage is FROM {:?}, want exactly {image}",
            build.image
        ))),
        BuildBase::Repository(_) | BuildBase::Exact(_) => {}
    }
    let instructions = build.joined();
    for want in rules.required {
        if !instructions.contains(want) {
            problems.push(Problem::new(format!("the build stage lacks {want:?}")));
        }
    }
    let args = stage_args(build);
    for download in rules.downloads {
        problems.extend(download_problems(build, &args, download));
    }
    for instruction in &build.instructions {
        match instruction.keyword.as_str() {
            "RUN" => problems.extend(run_fetch_problems(&instruction.args, rules.downloads)),
            "ADD" if add_fetches(&instruction.args) => problems.push(Problem::new(format!(
                "the build stage fetches with ADD {}, which nothing checks by sha256",
                instruction.args
            ))),
            _ => {}
        }
    }
    problems
}

fn download_problems(
    build: &Stage,
    args: &BTreeMap<&str, Option<&str>>,
    download: &Download<'_>,
) -> Vec<Problem> {
    let tool = download.tool;
    let mut problems = Vec::new();
    if download.arms.is_empty() {
        problems.push(Problem::new(format!(
            "the download rule of {tool} lists no arm; it would check nothing"
        )));
    }
    let declared = args.get(download.version_arg).copied().flatten();
    if declared != Some(download.version.as_str()) {
        problems.push(Problem::new(format!(
            "the build stage sets {} to {declared:?}, want {} for {tool}",
            download.version_arg, download.version
        )));
    }
    if !args.contains_key(download.platform_arg) {
        problems.push(Problem::new(format!(
            "the build stage does not declare ARG {}, which {tool} is chosen by",
            download.platform_arg
        )));
    }
    let runs = runs_with(build, download.url);
    let [run] = runs.as_slice() else {
        problems.push(Problem::new(format!(
            "{} RUN instructions download {tool} from {}, want one",
            runs.len(),
            download.url
        )));
        return problems;
    };
    if !run.starts_with(&format!("case \"${{{}}}\" in ", download.platform_arg)) {
        problems.push(Problem::new(format!(
            "the RUN that downloads {tool} does not switch on ${}",
            download.platform_arg
        )));
    }
    let arm = pattern(&format!(
        r"(?:^|[\s;])([A-Za-z0-9_*]+)\)\s*{}=([^;\s]+);\s*{}=([^;\s]+)",
        regex::escape(download.asset_var),
        regex::escape(download.sha_var)
    ));
    let found: BTreeSet<(String, String, String)> = arm
        .captures_iter(run)
        .map(|arm| (arm[1].to_owned(), arm[2].to_owned(), arm[3].to_owned()))
        .collect();
    let want: BTreeSet<(String, String, String)> = download.arms.iter().cloned().collect();
    if found != want {
        problems.push(Problem::new(format!(
            "{tool} is chosen by the arms {found:?}, want exactly {want:?}"
        )));
    }
    // The reader collapses whitespace runs, so the two spaces sha256sum reads are one here.
    let check = format!("echo \"${{{}}} /tmp/", download.sha_var);
    if !run.contains(&check) || !run.contains(SHA256_CHECK) {
        problems.push(Problem::new(format!(
            "the RUN that downloads {tool} does not check it with sha256sum -c against ${}",
            download.sha_var
        )));
    }
    problems
}

/// The version stamps: the variables `<prefix>_<field>` a version module reads at compile time,
/// set by the image's build stage from its build arguments and by make-common's `rust.mk` from
/// its git values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamps<'stamps> {
    /// The prefix, such as `SWARM_DEVICE_ACCESS`; the Makefile sets `RUST_STAMP_PREFIX` to it.
    pub prefix: &'stamps str,
    /// Each field and the make variable `rust.mk` stamps it from, such as
    /// `("VERSION", "GIT_VERSION")`; the build stage stamps it from `${<field>}`.
    pub fields: &'stamps [(&'stamps str, &'stamps str)],
    /// The version module, such as `src/version.rs`, named in a problem.
    pub version_file: &'stamps str,
    /// What the build stage's one building RUN runs, such as `cargo zigbuild`: the stamps must
    /// sit in that RUN, where the build reads them.
    pub build_command: &'stamps str,
}

/// make-common's stamp fields.
pub const STAMP_FIELDS: [(&str, &str); 3] = [
    ("VERSION", "GIT_VERSION"),
    ("COMMIT", "GIT_COMMIT"),
    ("DATE", "BUILD_DATE"),
];

/// The texts the stamps are checked in.
#[derive(Clone, Copy, Debug)]
pub struct StampSources<'sources> {
    /// The version module's text.
    pub version_rs: &'sources str,
    /// The image's build stage.
    pub build: &'sources Stage,
    /// The Makefile's text.
    pub makefile: &'sources str,
    /// make-common's `rust.mk`.
    pub rust_mk: &'sources str,
    /// A release fragment and the variable of its build recipe that must carry
    /// `$(RUST_VERSION_ENV)`, such as make-common's `.mk/rust-release.mk` and
    /// `rust_static_build`; `None` for a project without one.
    pub release: Option<(&'sources str, &'sources str)>,
}

/// A `RUST_STAMP_PREFIX` assignment of any flavour, `export` and `override` included.
static STAMP_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?m)^(?:(?:export|override)[ \t]+)*RUST_STAMP_PREFIX[ \t]*(?:[:?+!]|::)?=.*$")
});
/// The `export` and `override` words before an assignment.
static ASSIGNMENT_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^(?:(?:export|override)[ \t]+)*"));
/// `rust.mk`'s stamp assignment list: the variable and its continuation lines.
static VERSION_ENV: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^RUST_VERSION_ENV[ \t]*=(.*\\\n(?:.*\\\n)*.*)$"));

/// The recipe line under make's last rule for `target` (a line starting `<target>:`), or `None`.
fn last_recipe<'text>(text: &'text str, target: &str) -> Option<&'text str> {
    let rule = format!("{target}:");
    let lines: Vec<&str> = text.lines().collect();
    let index = lines.iter().rposition(|line| line.starts_with(&rule))?;
    lines.get(index + 1).copied()
}

/// The lines of make's last `<variable> =` assignment block in `text`, up to the first blank
/// line, without its comment lines.
fn assignment_block<'text>(text: &'text str, variable: &str) -> Vec<&'text str> {
    let lines: Vec<&str> = text.lines().collect();
    let starts = |line: &&str| {
        line.strip_prefix(variable).is_some_and(|rest| {
            rest.trim_start().starts_with('=') || rest.trim_start().starts_with(":=")
        })
    };
    let Some(index) = lines.iter().rposition(starts) else {
        return Vec::new();
    };
    lines[index..]
        .iter()
        .take_while(|line| !line.trim().is_empty())
        .filter(|line| !line.trim_start().starts_with('#'))
        .copied()
        .collect()
}

impl Stamps<'_> {
    /// The problem with the Makefile's winning `RUST_STAMP_PREFIX` assignment, if any.
    fn prefix_problem(&self, makefile: &str) -> Option<Problem> {
        let prefix_line = format!("RUST_STAMP_PREFIX := {}", self.prefix);
        // `export` and `override` keep the value; a `+=` appends to it and an `!=` replaces it
        // with a command's output, so either, winning, is a value other than the prefix.
        let assignments: Vec<&str> = STAMP_PREFIX
            .find_iter(makefile)
            .map(|line| {
                let prefix = ASSIGNMENT_PREFIX
                    .find(line.as_str())
                    .map_or(0, |found| found.end());
                &line.as_str()[prefix..]
            })
            .collect();
        // A `?=` after another assignment does not change the value; any other flavour does.
        let winning = assignments
            .iter()
            .rev()
            .find(|line| !line.contains("?="))
            .or_else(|| assignments.last());
        match winning {
            Some(line) if *line == prefix_line => None,
            Some(line) => Some(Problem::new(format!(
                "the Makefile's RUST_STAMP_PREFIX assignment that wins is {line:?}, want exactly \
                 {prefix_line:?}"
            ))),
            None => Some(Problem::new(format!(
                "the Makefile does not set {prefix_line}"
            ))),
        }
    }
}

impl Stamps<'_> {
    /// What is wrong with the stamps in `sources`: the version module reads each variable with
    /// `option_env!`; the build stage declares each field's argument and sets each variable from
    /// it inside the one RUN that runs the build command; the Makefile's last `RUST_STAMP_PREFIX`
    /// assignment is exactly `RUST_STAMP_PREFIX := <prefix>` (make keeps a trailing space in the
    /// value); `rust.mk`'s `RUST_VERSION_ENV` stamps each field from its make variable and the
    /// recipe of its last `rust-build` rule starts with `$(RUST_VERSION_ENV)`; and the release
    /// recipe, if any, carries it. Comments never count. A path that stamped nothing would ship
    /// a binary reporting its unstamped default.
    #[must_use]
    pub fn problems(&self, sources: &StampSources<'_>) -> Vec<Problem> {
        let mut problems = Vec::new();
        if self.fields.is_empty() {
            problems.push(Problem::new(
                "no stamp field; the check would pass on nothing",
            ));
        }
        problems.extend(self.prefix_problem(sources.makefile));
        let stamped_build = last_recipe(sources.rust_mk, "rust-build")
            .is_some_and(|recipe| recipe.starts_with("\t$(RUST_VERSION_ENV) "));
        if !stamped_build {
            problems.push(Problem::new(
                "the rust-build recipe does not start with $(RUST_VERSION_ENV)",
            ));
        }
        let version_env = VERSION_ENV
            .captures_iter(sources.rust_mk)
            .last()
            .and_then(|captures| captures.get(1))
            .map_or("", |block| block.as_str());
        if version_env.is_empty() {
            problems.push(Problem::new("rust.mk defines no RUST_VERSION_ENV"));
        }
        if let Some((release_mk, variable)) = sources.release {
            let carries = assignment_block(release_mk, variable)
                .iter()
                .any(|line| line.contains("$(RUST_VERSION_ENV)"));
            if !carries {
                problems.push(Problem::new(format!(
                    "the release recipe {variable} does not carry $(RUST_VERSION_ENV)"
                )));
            }
        }
        let runs = runs_with(sources.build, self.build_command);
        let run = match runs.as_slice() {
            [run] => Some(*run),
            runs => {
                problems.push(Problem::new(format!(
                    "{} RUN instructions of the build stage run {}, want one",
                    runs.len(),
                    self.build_command
                )));
                None
            }
        };
        let args = stage_args(sources.build);
        for (field, source) in self.fields {
            let variable = format!("{}_{field}", self.prefix);
            if !sources
                .version_rs
                .contains(&format!("option_env!(\x22{variable}\x22)"))
            {
                problems.push(Problem::new(format!(
                    "{} does not read {variable}",
                    self.version_file
                )));
            }
            if !args.contains_key(field) {
                problems.push(Problem::new(format!(
                    "the build stage does not declare ARG {field}"
                )));
            }
            if let Some(run) = run
                && !run.contains(&format!("{variable}=\x22${{{field}}}\x22"))
            {
                problems.push(Problem::new(format!(
                    "the RUN that runs {} does not stamp {variable} from ${field}",
                    self.build_command
                )));
            }
            if !version_env.contains(&format!("$(RUST_STAMP_PREFIX)_{field}=\x22$({source})\x22")) {
                problems.push(Problem::new(format!(
                    "rust.mk's RUST_VERSION_ENV does not stamp $(RUST_STAMP_PREFIX)_{field} from \
                     $({source})"
                )));
            }
        }
        problems
    }
}

/// A runtime stage's rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeStage<'rules> {
    /// The repository of its base, such as `dhi.io/static`.
    pub repository: &'rules str,
    /// Keywords it may not carry: `RUN`, `SHELL`, `VOLUME`, `WORKDIR`, `ADD`, `ENV`, and where the
    /// image has none, `HEALTHCHECK` and `EXPOSE`.
    pub forbidden: &'rules [&'rules str],
    /// Keywords it carries exactly once with exactly these arguments: `USER`, `ENTRYPOINT`, the
    /// `COPY` of the binary.
    pub exactly: &'rules [(&'rules str, &'rules str)],
}

/// What is wrong with a runtime stage against `rules`, which also follows the target platform
/// (no `--platform`).
#[must_use]
pub fn runtime_problems(runtime: &Stage, rules: &RuntimeStage<'_>) -> Vec<Problem> {
    if rules.forbidden.is_empty() && rules.exactly.is_empty() {
        return vec![Problem::new(
            "the runtime rules forbid nothing and fix nothing; the check would pass on almost \
             nothing",
        )];
    }
    let mut problems = Vec::new();
    if dockerfile::repository(&runtime.image) != rules.repository {
        problems.push(Problem::new(format!(
            "the runtime stage is FROM {:?}, want {}",
            runtime.image, rules.repository
        )));
    }
    if !runtime.platform.is_empty() {
        problems.push(Problem::new(format!(
            "the runtime stage pins --platform={}; it has to follow the target platform",
            runtime.platform
        )));
    }
    let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for instruction in &runtime.instructions {
        if rules.forbidden.contains(&instruction.keyword.as_str()) {
            problems.push(Problem::new(format!(
                "the runtime stage carries {} {}",
                instruction.keyword, instruction.args
            )));
        }
        seen.entry(instruction.keyword.as_str())
            .or_default()
            .push(instruction.args.as_str());
    }
    for (keyword, args) in rules.exactly {
        if seen.get(keyword).map(Vec::as_slice) != Some([*args].as_slice()) {
            problems.push(Problem::new(format!(
                "the runtime stage's {keyword} is {:?}, want exactly {keyword} {args}",
                seen.get(keyword)
            )));
        }
    }
    problems
}

/// What is wrong unless the labels of `stage` are exactly `want`: each key with its value, or
/// with any value that is not empty (`None`, such as a description). An empty `want` is a
/// problem.
#[must_use]
pub fn label_problems(stage: &Stage, want: &[(&str, Option<&str>)]) -> Vec<Problem> {
    if want.is_empty() {
        return vec![Problem::new(format!(
            "no label is wanted of the {} stage; the check would pass on nothing",
            stage.name
        ))];
    }
    let labels = dockerfile::labels(stage);
    let mut problems = Vec::new();
    for (key, value) in want {
        match (labels.get(*key), value) {
            (Some(found), None) if !found.is_empty() => {}
            (Some(found), Some(value)) if found == value => {}
            (found, value) => problems.push(Problem::new(format!(
                "the {} stage's label {key} is {found:?}, want {}",
                stage.name,
                value.map_or_else(|| "a value".to_owned(), |value| format!("{value:?}"))
            ))),
        }
    }
    for key in labels.keys() {
        if !want.iter().any(|(wanted, _)| wanted == key) {
            problems.push(Problem::new(format!(
                "the {} stage carries the label {key}, which is not one of the wanted labels",
                stage.name
            )));
        }
    }
    problems
}

/// A `platforms:` or `PLATFORMS:` declaration in a workflow, with its value.
static PLATFORM_DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?mi)^\s*(?:-\s*)?(platforms?)\s*:\s*(.+)$"));

/// What is wrong with the platforms `workflows` (path and text) declare: `declaring` (such as
/// `.github/workflows/release.yaml`, whose `PLATFORMS:` the image is built for) is one of them and
/// declares a literal value, each literal declaration anywhere is exactly `want` (an expression
/// is not a declaration), and none of `forbidden` is named anywhere.
#[must_use]
pub fn platform_problems(
    workflows: &[(String, String)],
    declaring: &str,
    want: &str,
    forbidden: &[&str],
) -> Vec<Problem> {
    let mut problems = Vec::new();
    match workflows.iter().find(|(file, _)| file == declaring) {
        None => problems.push(Problem::at(
            declaring,
            "is not among the workflows checked; the check would pass on nothing",
        )),
        Some((_, content)) => {
            let literal = PLATFORM_DECLARATION.captures_iter(content).any(|captures| {
                !captures[2]
                    .trim()
                    .trim_matches(['"', '\''])
                    .starts_with("${{")
            });
            if !literal {
                problems.push(Problem::at(
                    declaring,
                    "declares no literal platforms; the check would pass on nothing",
                ));
            }
        }
    }
    for (workflow, content) in workflows {
        for captures in PLATFORM_DECLARATION.captures_iter(content) {
            let value = captures[2].trim().trim_matches(['"', '\'']);
            if value.starts_with("${{") {
                continue;
            }
            if value != want {
                problems.push(Problem::at(
                    workflow,
                    format!("declares {} {value:?}, want {want:?}", &captures[1]),
                ));
            }
        }
        for platform in forbidden {
            if content.contains(platform) {
                problems.push(Problem::at(
                    workflow,
                    format!("names {platform}, a platform other than {want}"),
                ));
            }
        }
    }
    problems
}

/// What is wrong with the label policy: `droast` (the text of `droast_file`) must refuse any label
/// outside its `[required-labels]` (`strict-labels = true`), leave the severity of a missing
/// label (DF074) as droast ships it, and require exactly the labels the last stage of
/// `dockerfile_text` (the text of `dockerfile_file`) carries.
#[must_use]
pub fn label_policy_problems(
    dockerfile_file: &str,
    dockerfile_text: &str,
    droast_file: &str,
    droast: &str,
) -> Vec<Problem> {
    let config: Table = match droast.parse() {
        Ok(config) => config,
        Err(error) => return vec![Problem::at(droast_file, format!("parse: {error}"))],
    };
    let mut problems = Vec::new();
    if config.get("strict-labels").and_then(toml::Value::as_bool) != Some(true) {
        problems.push(Problem::at(
            droast_file,
            "does not set strict-labels = true, so droast accepts any label",
        ));
    }
    if let Some(severity) = config
        .get("severity-overrides")
        .and_then(|overrides| overrides.get("DF074"))
    {
        problems.push(Problem::at(
            droast_file,
            format!(
                "overrides the severity of DF074 (a missing required label) to {severity}, so a \
                 missing label may no longer fail"
            ),
        ));
    }
    let required: BTreeSet<String> = config
        .get("required-labels")
        .and_then(toml::Value::as_table)
        .map(|labels| labels.keys().cloned().collect())
        .unwrap_or_default();
    if required.is_empty() {
        problems.push(Problem::at(droast_file, "requires no label"));
    }
    let carried: BTreeSet<String> = match dockerfile::parse(dockerfile_text) {
        Ok(stages) => stages
            .last()
            .map(|runtime| dockerfile::labels(runtime).into_keys().collect())
            .unwrap_or_default(),
        Err(error) => return vec![Problem::at(dockerfile_file, error.to_string())],
    };
    for label in required.difference(&carried) {
        problems.push(Problem::at(
            droast_file,
            format!("requires {label}, which the runtime stage does not carry"),
        ));
    }
    for label in carried.difference(&required) {
        problems.push(Problem::at(
            dockerfile_file,
            format!("the runtime stage carries {label}, which {droast_file} does not require"),
        ));
    }
    problems
}

/// Whether the last component of `path` names a Dockerfile: `Dockerfile`, `<name>.Dockerfile` or
/// `Dockerfile.<name>`, but not `Dockerfile.<name>.dockerignore`.
#[must_use]
pub fn is_dockerfile_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_dockerfile)
}

#[cfg(test)]
mod tests;
