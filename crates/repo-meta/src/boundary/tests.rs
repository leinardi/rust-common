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

//! The boundary check on planted `cargo metadata` output: each way two copies can meet is found
//! and named, and a graph without one passes.

use serde_json::{Value, json};

use super::{Entry, Exception, Role, check_metadata, entry, metadata};
use crate::strings;

const URL: &str = "https://github.com/leinardi/rust-common";
const GIT: &str = "git+https://github.com/leinardi/rust-common?tag=v4.0.0#0123abcd";
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

const TABLE: &[Entry] = &[
    entry("clock", None, "jiff", Role::Guarded),
    entry(
        "service-kit",
        Some("process-metrics"),
        "prometheus-client",
        Role::Reexport("service_kit::prometheus_client"),
    ),
    entry(
        "service-kit",
        Some("http"),
        "tower-service",
        Role::Reexport("service_kit::tower_service"),
    ),
    entry(
        "service-kit",
        Some("http"),
        "libc",
        Role::Internal("constants"),
    ),
];

/// A dependency a manifest declares: name, rename, kind, target.
type Declared = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
);

/// A package: ID, name, version, source, declared dependencies, resolve edges, enabled features.
struct Pkg {
    id: &'static str,
    name: &'static str,
    version: &'static str,
    source: Option<&'static str>,
    declared: Vec<Declared>,
    edges: Vec<&'static str>,
    features: Vec<&'static str>,
}

fn pkg(
    id: &'static str,
    name: &'static str,
    version: &'static str,
    source: Option<&'static str>,
) -> Pkg {
    Pkg {
        id,
        name,
        version,
        source,
        declared: Vec::new(),
        edges: Vec::new(),
        features: Vec::new(),
    }
}

impl Pkg {
    fn declares(mut self, declared: Declared) -> Self {
        self.declared.push(declared);
        self
    }

    fn to(mut self, edges: &[&'static str]) -> Self {
        self.edges.extend(edges);
        self
    }

    fn with(mut self, features: &[&'static str]) -> Self {
        self.features.extend(features);
        self
    }
}

fn plain(name: &'static str) -> Declared {
    (name, None, None, None)
}

fn graph(packages: &[Pkg], members: &[&str]) -> Value {
    json!({
        "packages": packages.iter().map(|package| json!({
            "id": package.id,
            "name": package.name,
            "version": package.version,
            "source": package.source,
            "dependencies": package.declared.iter().map(|(name, rename, kind, target)| json!({
                "name": name,
                "rename": rename,
                "kind": kind,
                "target": target,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "workspace_members": members,
        "resolve": {
            "nodes": packages.iter().map(|package| json!({
                "id": package.id,
                "deps": package.edges.iter().map(|edge| json!({"pkg": edge})).collect::<Vec<_>>(),
                "features": package.features,
            })).collect::<Vec<_>>(),
            "root": null,
        },
    })
}

fn run(packages: &[Pkg], exceptions: &[Exception]) -> Vec<String> {
    strings(&check_metadata(
        &graph(packages, &["sda"]),
        TABLE,
        URL,
        exceptions,
    ))
}

fn service_kit(features: &[&'static str], edges: &[&'static str]) -> Pkg {
    pkg("sk", "service-kit", "4.0.0", Some(GIT))
        .with(features)
        .to(edges)
}

fn prometheus(id: &'static str, version: &'static str, source: &'static str) -> Pkg {
    pkg(id, "prometheus-client", version, Some(source))
}

#[test]
fn one_copy_taken_through_the_reexport_passes() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None)
                .to(&["sk", "jiff1"])
                .declares(plain("jiff")),
            service_kit(&["process-metrics"], &["prom26"]),
            prometheus("prom26", "0.26.0", REGISTRY),
            pkg("clock", "clock", "4.0.0", Some(GIT)).to(&["jiff1"]),
            pkg("jiff1", "jiff", "1.0.3", Some(REGISTRY)),
        ],
        &[],
    );
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_split_at_two_versions_names_both_and_the_reexport() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None)
                .to(&["sk", "prom26"])
                .declares(plain("prometheus-client")),
            service_kit(&["process-metrics"], &["prom25"]),
            prometheus("prom25", "0.25.2", REGISTRY),
            prometheus("prom26", "0.26.0", REGISTRY),
        ],
        &[],
    );
    assert_eq!(
        problems,
        [
            "2 copies of prometheus-client: 0.25.2 (crates.io), rust-common's, via sda -> \
             service-kit -> prometheus-client; 0.26.0 (crates.io) via sda -> prometheus-client; \
             types from one do not implement the other's traits (E0277/E0308); take it through \
             service_kit::prometheus_client",
            "sda declares prometheus-client directly (normal dependency); take it through \
             service_kit::prometheus_client, or pass an Exception with the reason",
        ]
    );
}

#[test]
fn the_same_version_from_another_source_is_a_split() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None).to(&["sk", "prom-git"]),
            service_kit(&["process-metrics"], &["prom26"]),
            prometheus("prom26", "0.26.0", REGISTRY),
            prometheus(
                "prom-git",
                "0.26.0",
                "git+https://github.com/prometheus/client_rust#feedface",
            ),
        ],
        &[],
    );
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(
        problems[0].starts_with(
            "2 copies of prometheus-client: 0.26.0 \
             (git+https://github.com/prometheus/client_rust#feedface) via sda -> \
             prometheus-client; 0.26.0 (crates.io), rust-common's, via sda -> service-kit -> \
             prometheus-client;"
        ),
        "{problems:#?}"
    );
}

#[test]
fn a_renamed_or_target_specific_declaration_is_found() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None)
                .to(&["sk", "prom26"])
                .declares(("prometheus-client", Some("prom"), None, None))
                .declares(("prometheus-client", None, Some("dev"), Some("cfg(unix)"))),
            service_kit(&["process-metrics"], &["prom26"]),
            prometheus("prom26", "0.26.0", REGISTRY),
        ],
        &[],
    );
    assert_eq!(
        problems,
        [
            "sda declares prometheus-client directly (normal dependency, renamed prom); take it \
             through service_kit::prometheus_client, or pass an Exception with the reason",
            "sda declares prometheus-client directly (dev dependency, for cfg(unix)); take it \
             through service_kit::prometheus_client, or pass an Exception with the reason",
        ]
    );
}

#[test]
fn a_transitive_split_through_a_third_party_crate_is_found() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None).to(&["sk", "axum"]),
            service_kit(&["http"], &["tower333", "libc"]),
            pkg("axum", "axum", "0.8.9", Some(REGISTRY)).to(&["tower332"]),
            pkg("tower333", "tower-service", "0.3.3", Some(REGISTRY)),
            pkg("tower332", "tower-service", "0.3.2", Some(REGISTRY)),
            pkg("libc", "libc", "0.2.191", Some(REGISTRY)),
        ],
        &[],
    );
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(
        problems[0].starts_with(
            "2 copies of tower-service: 0.3.2 (crates.io) via sda -> axum -> tower-service; \
             0.3.3 (crates.io), rust-common's, via sda -> service-kit -> tower-service;"
        ),
        "{problems:#?}"
    );
}

#[test]
fn a_guarded_split_asks_for_rust_commons_version() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None)
                .to(&["clock", "jiff02"])
                .declares(plain("jiff")),
            pkg("clock", "clock", "4.0.0", Some(GIT)).to(&["jiff1"]),
            pkg("jiff1", "jiff", "1.0.3", Some(REGISTRY)),
            pkg("jiff02", "jiff", "0.2.38", Some(REGISTRY)),
        ],
        &[],
    );
    assert_eq!(
        problems,
        [
            "2 copies of jiff: 0.2.38 (crates.io) via sda -> jiff; 1.0.3 (crates.io), rust-common's, \
          via sda -> clock -> jiff; types from one do not implement the other's traits \
          (E0277/E0308); require the one rust-common uses, 1.0.3 (crates.io)"
        ]
    );
}

#[test]
fn an_exception_allows_the_declaration_and_a_stale_one_is_a_problem() {
    let used = Exception {
        member: "sda",
        package: "prometheus-client",
        reason: "a test",
    };
    let stale = Exception {
        member: "sda",
        package: "tower-service",
        reason: "gone",
    };
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None)
                .to(&["sk", "prom26"])
                .declares(plain("prometheus-client")),
            service_kit(&["process-metrics"], &["prom26"]),
            prometheus("prom26", "0.26.0", REGISTRY),
        ],
        &[used, stale],
    );
    assert_eq!(
        problems,
        [
            "the exception for sda declaring tower-service matches no direct declaration of an \
          active re-exported package: remove it"
        ]
    );
}

#[test]
fn an_excepted_declaration_is_still_under_the_identity_check() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None)
                .to(&["sk", "prom25"])
                .declares(plain("prometheus-client")),
            service_kit(&["process-metrics"], &["prom26"]),
            prometheus("prom26", "0.26.0", REGISTRY),
            prometheus("prom25", "0.25.2", REGISTRY),
        ],
        &[Exception {
            member: "sda",
            package: "prometheus-client",
            reason: "a test",
        }],
    );
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(
        problems[0].starts_with("2 copies of prometheus-client"),
        "{problems:#?}"
    );
}

#[test]
fn an_inactive_feature_checks_nothing() {
    // bitgauge: service-kit without process-metrics, and its own prometheus-client.
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None)
                .to(&["sk", "prom24"])
                .declares(plain("prometheus-client")),
            service_kit(&[], &[]),
            prometheus("prom24", "0.24.0", REGISTRY),
        ],
        &[],
    );
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn an_active_entry_rust_common_does_not_depend_on_is_a_stale_table() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None).to(&["sk"]),
            service_kit(&["process-metrics"], &[]),
        ],
        &[],
    );
    assert_eq!(
        problems,
        [
            "the boundary table names prometheus-client under service-kit (feature \
          process-metrics), but service-kit 4.0.0 \
          (git+https://github.com/leinardi/rust-common?tag=v4.0.0#0123abcd) does not depend on \
          it here: the table is out of date"
        ]
    );
}

#[test]
fn rust_common_under_a_path_patch_is_still_rust_common() {
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None).to(&["sk", "prom25"]),
            pkg("sk", "service-kit", "4.0.0", None)
                .with(&["process-metrics"])
                .to(&["prom26"]),
            prometheus("prom26", "0.26.0", REGISTRY),
            prometheus("prom25", "0.25.2", REGISTRY),
        ],
        &[],
    );
    assert_eq!(problems.len(), 1, "{problems:#?}");
}

#[test]
fn nothing_to_check_is_a_problem() {
    // No rust-common package: a crate of the same name from crates.io is not one.
    let problems = run(
        &[
            pkg("sda", "sda", "0.1.0", None).to(&["clock"]),
            pkg("clock", "clock", "0.1.0", Some(REGISTRY)),
        ],
        &[],
    );
    assert_eq!(
        problems,
        [
            "no rust-common package from https://github.com/leinardi/rust-common is in the \
          dependency graph: the check would pass on nothing"
        ]
    );
    // An empty table.
    let packages = [pkg("sda", "sda", "0.1.0", None)];
    assert_eq!(
        strings(&check_metadata(&graph(&packages, &["sda"]), &[], URL, &[])),
        ["the boundary table is empty: the check would pass on nothing"]
    );
    // Input that is not cargo metadata, or has no resolve graph.
    assert_eq!(
        strings(&check_metadata(&json!({}), TABLE, URL, &[])),
        ["cargo metadata: the output has no \"packages\" array"]
    );
    assert_eq!(
        strings(&check_metadata(
            &json!({"packages": [], "workspace_members": [], "resolve": null}),
            TABLE,
            URL,
            &[]
        )),
        ["cargo metadata: no resolve graph"]
    );
}

#[test]
fn a_workspace_without_a_lock_is_a_problem() {
    let scratch = tempfile::tempdir().unwrap();
    let problem = metadata(scratch.path()).unwrap_err();
    assert_eq!(
        problem.to_string(),
        "Cargo.lock: missing: the boundary check reads the locked graph"
    );
}

#[test]
fn a_url_written_with_git_or_a_slash_matches() {
    assert!(super::from_rust_common(
        GIT,
        "https://github.com/leinardi/rust-common.git"
    ));
    assert!(super::from_rust_common(
        GIT,
        "https://github.com/leinardi/rust-common/"
    ));
    assert!(!super::from_rust_common(
        GIT,
        "https://github.com/leinardi/rust-common-fork"
    ));
    assert!(!super::from_rust_common(REGISTRY, URL));
}
