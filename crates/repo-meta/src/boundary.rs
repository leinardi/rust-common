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

//! The crates whose types cross rust-common's public API resolve to one package everywhere in a
//! consumer's graph. Two copies of such a crate (two versions, or one version from two sources)
//! compile apart and fail at the boundary with errors that name neither copy: a type of one does
//! not implement the other's traits (E0277) or is not the other's type (E0308). This module names
//! the real problem instead.
//!
//! The [`TABLE`] lists, for each rust-common crate and the feature that brings the dependency in,
//! every third-party dependency it has and its role at the boundary:
//!
//! - [`Role::Reexport`]: a pre-1.0 crate whose types appear in the public API, re-exported at the
//!   crate's root (`pub use X;`) and part of that crate's semver. A consumer imports it through the
//!   re-export and does not declare it, except under an [`Exception`] it passes with a reason.
//! - [`Role::Guarded`]: a crate whose types cross the API but which is not re-exported (a 1.x
//!   crate, whose compatible bumps a consumer may take, or one a consumer keeps by choice).
//! - [`Role::Internal`]: a crate no type of which crosses the API, with the reason.
//!
//! [`check`] reads `cargo metadata --format-version 1 --locked --all-features` (package IDs and
//! resolve edges, never the lock text) and runs two checks for every entry that is active, that
//! is whose rust-common crate is in the graph with the entry's feature enabled:
//!
//! 1. **Identity.** Every package of a `Reexport` or `Guarded` entry's name in the graph is the
//!    package ID rust-common uses, transitive copies included (axum's tower-service, say). A
//!    problem names each copy, a dependency path that reaches it and the import to use.
//! 2. **Declaration.** No workspace member declares an active `Reexport` package directly
//!    (renamed, target-specific, dev and build declarations included), except under an
//!    [`Exception`]. A package so excepted is still under the identity check.
//!
//! The check never passes on nothing: an empty table, no rust-common package in the graph, a
//! missing lock, metadata that fails or does not parse, and an exception that matches no
//! declaration are each a problem. An entry whose feature is off is inactive and checks nothing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::Problem;

/// A dependency's role at rust-common's public API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Re-exported at the crate's root, by this path (`service_kit::prometheus_client`).
    Reexport(&'static str),
    /// Its types cross the API, but it is not re-exported: under the identity check only.
    Guarded,
    /// No type of it crosses the API, for this reason.
    Internal(&'static str),
}

/// One dependency of one rust-common crate, under the feature that brings it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The rust-common package (`service-kit`).
    pub krate: &'static str,
    /// The feature of `krate` that brings the dependency in, `None` for a dependency that is
    /// always there.
    pub feature: Option<&'static str>,
    /// The dependency's package name (`prometheus-client`): a direct dependency of `krate`, or one
    /// in its closure (futures-core, through futures-util).
    pub package: &'static str,
    /// Its role.
    pub role: Role,
}

/// A consumer's leave to declare a re-exported package directly: the workspace member, the
/// package and why. Such a package is still under the identity check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exception {
    /// The workspace member's package name.
    pub member: &'static str,
    /// The package it declares.
    pub package: &'static str,
    /// Why it declares it.
    pub reason: &'static str,
}

const fn entry(
    krate: &'static str,
    feature: Option<&'static str>,
    package: &'static str,
    role: Role,
) -> Entry {
    Entry {
        krate,
        feature,
        package,
        role,
    }
}

use Role::{Guarded, Internal, Reexport};

/// Every third-party dependency of every rust-common crate, with its role. rust-common's
/// meta-tests hold it to the manifests and to the crates' root re-exports, so a new dependency
/// or re-export is a decision recorded here.
pub const TABLE: &[Entry] = &[
    // clock
    entry("clock", None, "jiff", Guarded),
    entry("clock", None, "tokio", Guarded),
    // config-layer
    entry("config-layer", None, "jiff", Guarded),
    entry(
        "config-layer",
        None,
        "serde_core",
        Internal("the visitor the YAML is read with; no serde type is in the API"),
    ),
    entry(
        "config-layer",
        None,
        "serde-saphyr",
        Internal("the YAML parser; its spans are copied into the crate's own Location"),
    ),
    entry(
        "config-layer",
        None,
        "granit-parser",
        Internal("held only for its version, under serde-saphyr"),
    ),
    entry("config-layer", None, "thiserror", Internal("derives only")),
    // docker-client
    entry(
        "docker-client",
        None,
        "bollard",
        Reexport("docker_client::bollard"),
    ),
    entry("docker-client", None, "http", Guarded),
    entry(
        "docker-client",
        None,
        "futures-util",
        Internal("only its re-export of futures-core's Stream crosses the API"),
    ),
    // Guarded though pre-1.0: Stream reaches the API through futures-util's re-export.
    entry("docker-client", None, "futures-core", Guarded),
    entry("docker-client", None, "thiserror", Internal("derives only")),
    entry("docker-client", None, "tokio", Guarded),
    // Guarded though pre-1.0: only CancellationToken crosses, re-exported as a type
    // (docker_client::CancellationToken); the crate is re-exported by service-kit (http) alone.
    entry("docker-client", None, "tokio-util", Guarded),
    entry("docker-client", None, "bytes", Guarded),
    entry(
        "docker-client",
        None,
        "http-body-util",
        Internal("the request and response bodies, built and read inside the crate"),
    ),
    entry("docker-client", None, "hyper", Guarded),
    entry(
        "docker-client",
        None,
        "hyper-util",
        Internal("TokioIo, wrapping the crate's own connections"),
    ),
    entry(
        "docker-client",
        None,
        "serde",
        Internal("the refusal's message, read inside the crate"),
    ),
    entry("docker-client", None, "serde_json", Guarded),
    entry(
        "docker-client",
        None,
        "serde_urlencoded",
        Internal("the query strings the crate builds"),
    ),
    entry(
        "docker-client",
        Some("tls"),
        "rustls",
        Internal("the TLS connector, built from the config's paths"),
    ),
    entry(
        "docker-client",
        Some("tls"),
        "rustls-native-certs",
        Internal("the system roots, read inside the crate"),
    ),
    entry(
        "docker-client",
        Some("tls"),
        "rustls-pki-types",
        Internal("the certificates and key, read inside the crate"),
    ),
    entry(
        "docker-client",
        Some("tls"),
        "tokio-rustls",
        Internal("the TLS stream, inside the crate's connections"),
    ),
    entry(
        "docker-client",
        Some("tcp"),
        "socket2",
        Internal("TCP keepalive on the crate's own sockets"),
    ),
    // repo-meta
    entry("repo-meta", None, "regex", Guarded),
    entry(
        "repo-meta",
        None,
        "serde-saphyr",
        Internal("the YAML parser; what it reads is a serde_json Value"),
    ),
    entry("repo-meta", None, "serde_json", Guarded),
    entry(
        "repo-meta",
        None,
        "tempfile",
        Internal("the scratch repositories' directories, owned by the crate"),
    ),
    entry("repo-meta", None, "toml", Guarded),
    // service-kit
    entry("service-kit", Some("runtime"), "tokio", Guarded),
    entry("service-kit", Some("http"), "tokio", Guarded),
    entry("service-kit", Some("http"), "bytes", Guarded),
    entry("service-kit", Some("http"), "hyper", Guarded),
    entry("service-kit", Some("http"), "http", Guarded),
    entry("service-kit", Some("http"), "http-body", Guarded),
    entry(
        "service-kit",
        Some("http"),
        "hyper-util",
        Internal("the server's I/O and timer adapters, inside the crate"),
    ),
    entry(
        "service-kit",
        Some("http"),
        "libc",
        Internal("the accept-error constants"),
    ),
    entry(
        "service-kit",
        Some("http"),
        "tokio-util",
        Reexport("service_kit::tokio_util"),
    ),
    entry(
        "service-kit",
        Some("http"),
        "tower-service",
        Reexport("service_kit::tower_service"),
    ),
    entry(
        "service-kit",
        Some("process-metrics"),
        "prometheus-client",
        Reexport("service_kit::prometheus_client"),
    ),
    // testenv-core
    entry("testenv-core", None, "jiff", Guarded),
    entry("testenv-core", None, "regex", Guarded),
    entry("testenv-core", Some("async"), "tokio", Guarded),
    // Guarded though pre-1.0: only CancellationToken crosses the async API (see docker-client).
    entry("testenv-core", Some("async"), "tokio-util", Guarded),
    entry(
        "testenv-core",
        Some("async"),
        "tempfile",
        Internal("the test's temporary directory, handed out as a path"),
    ),
    entry(
        "testenv-core",
        Some("sync"),
        "tempfile",
        Internal("the test's temporary directory, handed out as a path"),
    ),
    entry(
        "testenv-core",
        Some("bollard"),
        "bollard",
        Reexport("testenv_core::bollard"),
    ),
    entry(
        "testenv-core",
        Some("bollard"),
        "rustix",
        Internal("the sweep's PID liveness check"),
    ),
    entry(
        "testenv-core",
        Some("dind"),
        "futures-util",
        Internal("the image pull's progress stream, read inside the crate"),
    ),
    // tracing-setup
    // Guarded though pre-1.0: the consumers keep declaring tracing, by choice.
    entry("tracing-setup", None, "tracing", Guarded),
    entry(
        "tracing-setup",
        None,
        "tracing-subscriber",
        Reexport("tracing_setup::tracing_subscriber"),
    ),
    entry(
        "tracing-setup",
        Some("log-bridge"),
        "tracing-log",
        Internal("the LogTracer, installed by the crate"),
    ),
    entry("tracing-setup", Some("testing"), "serde_json", Guarded),
];

/// Runs the boundary check on the consumer workspace at `root`: its metadata, the rust-common
/// packages from `url` (the git repository a consumer depends on, or a path under `[patch]`), the
/// [`TABLE`] and the consumer's exceptions.
///
/// It runs `cargo` (the `CARGO` of a cargo-run test, else the one on the path) online, so a
/// dependency missing from the cache is fetched; a CI job runs `cargo fetch --locked` first, so a
/// fetch that fails fails there, with cargo's own error.
#[must_use]
pub fn check(root: &Path, url: &str, exceptions: &[Exception]) -> Vec<Problem> {
    match metadata(root) {
        Ok(metadata) => check_metadata(&metadata, TABLE, url, exceptions),
        Err(problem) => vec![problem],
    }
}

/// `cargo metadata --format-version 1 --locked --all-features` of the workspace at `root`, parsed.
///
/// # Errors
///
/// `Cargo.lock` is missing, cargo fails (an out-of-date lock included) or its output does not
/// parse.
pub fn metadata(root: &Path) -> Result<Value, Problem> {
    if !root.join("Cargo.lock").is_file() {
        return Err(Problem::at(
            "Cargo.lock",
            "missing: the boundary check reads the locked graph",
        ));
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--all-features",
        ])
        .current_dir(root)
        .output()
        .map_err(|error| Problem::new(format!("cargo metadata cannot be run: {error}")))?;
    if !output.status.success() {
        return Err(Problem::new(format!(
            "cargo metadata --locked --all-features failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| Problem::new(format!("cargo metadata's output does not parse: {error}")))
}

/// One package of the metadata.
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    declared: Vec<Declared>,
}

/// One dependency a package's manifest declares.
struct Declared {
    name: String,
    rename: Option<String>,
    kind: Option<String>,
    target: Option<String>,
}

/// The parts of the metadata the checks read.
struct Graph {
    packages: BTreeMap<String, Package>,
    /// Resolve edges, by package ID.
    edges: BTreeMap<String, Vec<String>>,
    /// Enabled features, by package ID.
    features: BTreeMap<String, BTreeSet<String>>,
    members: Vec<String>,
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn array<'value>(value: &'value Value, key: &str, what: &str) -> Result<&'value [Value], Problem> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| Problem::new(format!("cargo metadata: {what} has no {key:?} array")))
}

fn parse(metadata: &Value) -> Result<Graph, Problem> {
    let mut packages = BTreeMap::new();
    for package in array(metadata, "packages", "the output")? {
        let (Some(id), Some(name), Some(version)) = (
            string(package, "id"),
            string(package, "name"),
            string(package, "version"),
        ) else {
            return Err(Problem::new(
                "cargo metadata: a package lacks its id, name or version",
            ));
        };
        let declared = array(package, "dependencies", &format!("package {id}"))?
            .iter()
            .filter_map(|dependency| {
                Some(Declared {
                    name: string(dependency, "name")?,
                    rename: string(dependency, "rename"),
                    kind: string(dependency, "kind"),
                    target: string(dependency, "target"),
                })
            })
            .collect();
        packages.insert(
            id,
            Package {
                name,
                version,
                source: string(package, "source"),
                declared,
            },
        );
    }
    let resolve = metadata
        .get("resolve")
        .filter(|resolve| !resolve.is_null())
        .ok_or_else(|| Problem::new("cargo metadata: no resolve graph"))?;
    let mut edges = BTreeMap::new();
    let mut features = BTreeMap::new();
    for node in array(resolve, "nodes", "the resolve graph")? {
        let id = string(node, "id")
            .ok_or_else(|| Problem::new("cargo metadata: a resolve node has no id"))?;
        let targets = array(node, "deps", &format!("node {id}"))?
            .iter()
            .filter_map(|dependency| string(dependency, "pkg"))
            .collect();
        let enabled = node
            .get("features")
            .and_then(Value::as_array)
            .map(|enabled| {
                enabled
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        edges.insert(id.clone(), targets);
        features.insert(id, enabled);
    }
    let members = array(metadata, "workspace_members", "the output")?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Ok(Graph {
        packages,
        edges,
        features,
        members,
    })
}

/// `url` without a trailing slash or `.git`, so the forms a manifest may write compare equal.
fn normalize(url: &str) -> &str {
    let url = url.trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url)
}

/// Whether a package's source is the rust-common repository at `url` (any revision).
fn from_rust_common(source: &str, url: &str) -> bool {
    source.strip_prefix("git+").is_some_and(|rest| {
        let repository = rest.split(['?', '#']).next().unwrap_or(rest);
        normalize(repository) == normalize(url)
    })
}

/// A package ID as a problem names it: version and source.
fn describe(package: &Package) -> String {
    let source = match package.source.as_deref() {
        None => "path",
        Some(
            "registry+https://github.com/rust-lang/crates.io-index"
            | "sparse+https://index.crates.io/",
        ) => "crates.io",
        Some(other) => other,
    };
    format!("{} ({source})", package.version)
}

impl Graph {
    fn name<'graph>(&'graph self, id: &'graph str) -> &'graph str {
        self.packages
            .get(id)
            .map_or(id, |package| package.name.as_str())
    }

    /// Every package reachable from `start`, itself excluded.
    fn closure(&self, start: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([start.to_owned()]);
        while let Some(id) = queue.pop_front() {
            for next in self.edges.get(&id).into_iter().flatten() {
                if next != start && seen.insert(next.clone()) {
                    queue.push_back(next.clone());
                }
            }
        }
        seen
    }

    /// A shortest path from a workspace member to every package it reaches, as package names.
    fn paths(&self) -> BTreeMap<String, Vec<String>> {
        let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut queue = VecDeque::new();
        for member in &self.members {
            paths.insert(member.clone(), vec![self.name(member).to_owned()]);
            queue.push_back(member.clone());
        }
        while let Some(id) = queue.pop_front() {
            let here = paths.get(&id).cloned().unwrap_or_default();
            for next in self.edges.get(&id).into_iter().flatten() {
                if !paths.contains_key(next) {
                    let mut path = here.clone();
                    path.push(self.name(next).to_owned());
                    paths.insert(next.clone(), path);
                    queue.push_back(next.clone());
                }
            }
        }
        paths
    }
}

/// What rust-common uses of one boundary package, over every active entry.
#[derive(Default)]
struct Expected {
    ids: BTreeSet<String>,
    reexports: BTreeSet<&'static str>,
}

/// The two checks on parsed `cargo metadata` output, with `table` in place of [`TABLE`] (a test
/// plants its own).
#[must_use]
pub fn check_metadata(
    metadata: &Value,
    table: &[Entry],
    url: &str,
    exceptions: &[Exception],
) -> Vec<Problem> {
    if table.is_empty() {
        return vec![Problem::new(
            "the boundary table is empty: the check would pass on nothing",
        )];
    }
    let graph = match parse(metadata) {
        Ok(graph) => graph,
        Err(problem) => return vec![problem],
    };
    let krates: BTreeSet<&str> = table.iter().map(|entry| entry.krate).collect();
    let rust_common: Vec<&String> = graph
        .edges
        .keys()
        .filter(|id| {
            graph.packages.get(*id).is_some_and(|package| {
                krates.contains(package.name.as_str())
                    && match package.source.as_deref() {
                        Some(source) => from_rust_common(source, url),
                        // A path package outside the workspace: rust-common under [patch].
                        None => !graph.members.contains(*id),
                    }
            })
        })
        .collect();
    if rust_common.is_empty() {
        return vec![Problem::new(format!(
            "no rust-common package from {url} is in the dependency graph: the check would pass \
             on nothing"
        ))];
    }

    let mut problems = Vec::new();
    let mut expected: BTreeMap<&str, Expected> = BTreeMap::new();
    for id in &rust_common {
        let krate = graph.name(id);
        let enabled = graph.features.get(*id);
        let closure = graph.closure(id);
        for entry in table.iter().filter(|entry| entry.krate == krate) {
            let reexport = match entry.role {
                Role::Internal(_) => continue,
                Role::Reexport(path) => Some(path),
                Role::Guarded => None,
            };
            let active = entry
                .feature
                .is_none_or(|feature| enabled.is_some_and(|enabled| enabled.contains(feature)));
            if !active {
                continue;
            }
            let copies: Vec<&String> = closure
                .iter()
                .filter(|dependency| graph.name(dependency) == entry.package)
                .collect();
            if copies.is_empty() {
                problems.push(Problem::new(format!(
                    "the boundary table names {} under {krate}{}, but {} does not depend on it \
                     here: the table is out of date",
                    entry.package,
                    entry
                        .feature
                        .map(|feature| format!(" (feature {feature})"))
                        .unwrap_or_default(),
                    graph.packages.get(*id).map_or_else(
                        || krate.to_owned(),
                        |package| format!("{krate} {}", describe(package))
                    ),
                )));
                continue;
            }
            let slot = expected.entry(entry.package).or_default();
            slot.ids.extend(copies.into_iter().cloned());
            slot.reexports.extend(reexport);
        }
    }

    problems.extend(identity(&graph, &expected));
    problems.extend(declarations(&graph, &expected, exceptions));
    problems
}

/// Check 1: every package of a boundary name in the graph is the one rust-common uses.
fn identity(graph: &Graph, expected: &BTreeMap<&str, Expected>) -> Vec<Problem> {
    let paths = graph.paths();
    let mut problems = Vec::new();
    for (package, uses) in expected {
        let copies: BTreeSet<&String> = graph
            .edges
            .keys()
            .filter(|id| graph.name(id) == *package)
            .collect();
        if copies.len() <= 1 && uses.ids.len() <= 1 {
            continue;
        }
        let listed = copies
            .iter()
            .map(|id| {
                let via = paths.get(*id).map_or_else(
                    || "no workspace member".to_owned(),
                    |path| path.join(" -> "),
                );
                let ours = if uses.ids.contains(*id) {
                    ", rust-common's,"
                } else {
                    ""
                };
                let description = graph
                    .packages
                    .get(*id)
                    .map_or_else(|| (*id).clone(), describe);
                format!("{description}{ours} via {via}")
            })
            .collect::<Vec<_>>()
            .join("; ");
        let fix = if uses.reexports.is_empty() {
            let versions = uses
                .ids
                .iter()
                .filter_map(|id| graph.packages.get(id))
                .map(describe)
                .collect::<Vec<_>>()
                .join(" and ");
            format!("require the one rust-common uses, {versions}")
        } else {
            format!(
                "take it through {}",
                uses.reexports
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" or ")
            )
        };
        problems.push(Problem::new(format!(
            "{} copies of {package}: {listed}; types from one do not implement the other's traits \
             (E0277/E0308); {fix}",
            copies.len()
        )));
    }
    problems
}

/// Check 2: no workspace member declares an active re-exported package, outside an exception.
fn declarations(
    graph: &Graph,
    expected: &BTreeMap<&str, Expected>,
    exceptions: &[Exception],
) -> Vec<Problem> {
    let mut matched = vec![false; exceptions.len()];
    let mut problems = Vec::new();
    for member in &graph.members {
        let Some(package) = graph.packages.get(member) else {
            continue;
        };
        for declared in &package.declared {
            let Some(uses) = expected
                .get(declared.name.as_str())
                .filter(|uses| !uses.reexports.is_empty())
            else {
                continue;
            };
            if let Some(index) = exceptions.iter().position(|exception| {
                exception.member == package.name && exception.package == declared.name
            }) {
                if let Some(flag) = matched.get_mut(index) {
                    *flag = true;
                }
                continue;
            }
            let mut how = vec![format!(
                "{} dependency",
                declared.kind.as_deref().unwrap_or("normal")
            )];
            if let Some(rename) = &declared.rename {
                how.push(format!("renamed {rename}"));
            }
            if let Some(target) = &declared.target {
                how.push(format!("for {target}"));
            }
            problems.push(Problem::new(format!(
                "{} declares {} directly ({}); take it through {}, or pass an Exception with the \
                 reason",
                package.name,
                declared.name,
                how.join(", "),
                uses.reexports
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" or ")
            )));
        }
    }
    for (exception, matched) in exceptions.iter().zip(matched) {
        if !matched {
            problems.push(Problem::new(format!(
                "the exception for {} declaring {} matches no direct declaration of an active \
                 re-exported package: remove it",
                exception.member, exception.package
            )));
        }
    }
    problems
}

#[cfg(test)]
mod tests;
