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

//! The boundary table of `repo_meta::boundary` classifies every third-party dependency of every
//! crate and lists exactly the crates' root re-exports (`meta::boundary_table`). The negative
//! controls run the same check on a scratch tree with a planted, unclassified dependency, a stale
//! entry and a re-export the table lacks.

use meta::boundary_table::completeness;
use repo_meta::boundary::{Entry, Role, TABLE};
use repo_meta::scratch::ScratchRepo;
use repo_meta::{report, strings};

#[test]
fn the_boundary_table_matches_the_crates() {
    let problems = completeness(&meta::repo(), TABLE);
    assert!(problems.is_empty(), "{}", report(&problems));
}

const PLANTED: &[Entry] = &[
    Entry {
        krate: "alpha",
        feature: None,
        package: "jiff",
        role: Role::Guarded,
    },
    Entry {
        krate: "alpha",
        feature: Some("metrics"),
        package: "prometheus-client",
        role: Role::Reexport("alpha::prometheus_client"),
    },
];

fn scratch(lib_rs: &str, extra_dependency: &str) -> ScratchRepo {
    let scratch = ScratchRepo::new();
    scratch.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\", \"meta\"]\n",
    );
    scratch.write(
        "crates/alpha/Cargo.toml",
        &format!(
            "[package]\nname = \"alpha\"\n\n[features]\nmetrics = [\"dep:prometheus-client\"]\n\n\
             [dependencies]\njiff = \"1.0.3\"\nprometheus-client = {{ version = \"0.26.0\", \
             optional = true }}\nbeta = {{ path = \"../beta\" }}\n{extra_dependency}"
        ),
    );
    scratch.write("crates/alpha/src/lib.rs", lib_rs);
    scratch.write(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"alpha\"\nversion = \"0.1.0\"\ndependencies = [\n \
         \"jiff\",\n \"prometheus-client\",\n \"regex 1.13.1 (registry+https://x)\",\n]\n\n\
         [[package]]\nname = \"jiff\"\nversion = \"1.0.3\"\n\n[[package]]\nname = \
         \"prometheus-client\"\nversion = \"0.26.0\"\n",
    );
    scratch
}

const LIB_RS: &str = "//! Alpha.\n\n#[cfg(feature = \"metrics\")]\n/// The client.\npub use \
                      prometheus_client;\npub use inner::Thing;\n";

#[test]
fn a_table_that_matches_passes() {
    let scratch = scratch(LIB_RS, "");
    let problems = completeness(&scratch.repo(), PLANTED);
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn completeness_check_is_not_blind() {
    // An unclassified dependency, renamed; a re-export the table lacks; a Reexport entry no
    // re-export backs (its cfg dropped); an entry for a feature and a package the crate lacks.
    let lib_rs = "pub use prometheus_client;\npub use rx;\n";
    let scratch = scratch(
        lib_rs,
        "rx = { package = \"regex\", version = \"1.13.1\" }\n",
    );
    let mut table = PLANTED.to_vec();
    table.push(Entry {
        krate: "alpha",
        feature: Some("gone"),
        package: "tokio",
        role: Role::Guarded,
    });
    assert_eq!(
        strings(&completeness(&scratch.repo(), &table)),
        [
            "crates/alpha/Cargo.toml: regex is not in the boundary table under alpha: classify it \
             as Reexport, Guarded or Internal (repo_meta::boundary::TABLE)",
            "boundary entry (alpha, gone, tokio) names a feature alpha does not declare",
            "boundary entry (alpha, gone, tokio): alpha does not depend on tokio in Cargo.lock",
            "crates/alpha/src/lib.rs:1: pub use prometheus_client re-exports prometheus-client \
             with no Reexport(\"alpha::prometheus_client\") entry under that feature in the \
             boundary table",
            "crates/alpha/src/lib.rs:2: pub use rx re-exports regex with no \
             Reexport(\"alpha::rx\") entry under that feature in the boundary table",
        ]
    );
}

#[test]
fn a_reexport_entry_without_its_reexport_is_found() {
    let scratch = scratch("//! Alpha.\n", "");
    assert_eq!(
        strings(&completeness(&scratch.repo(), PLANTED)),
        ["boundary entry (alpha, metrics, prometheus-client) is \
          Reexport(\"alpha::prometheus_client\"), but crates/alpha/src/lib.rs has no pub use of \
          it at its root"]
    );
}

/// The check on real `cargo metadata` output, offline: a scratch consumer of this checkout's clock
/// (a path outside its workspace, as under `[patch]`) beside a local package also named jiff.
#[test]
fn the_check_reads_real_cargo_metadata() {
    let consumer = tempfile::tempdir().unwrap();
    let write = |relative: &str, content: &str| {
        let path = consumer.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    let clock = meta::repo().path("crates/clock");
    write(
        "Cargo.toml",
        &format!(
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nclock = {{ path = {:?} }}\njiff = {{ path = \"jiff\" }}\n",
            clock.display().to_string()
        ),
    );
    write("src/lib.rs", "");
    write(
        "jiff/Cargo.toml",
        "[package]\nname = \"jiff\"\nversion = \"0.2.0\"\nedition = \"2024\"\n",
    );
    write("jiff/src/lib.rs", "");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let lock = std::process::Command::new(cargo)
        .args(["generate-lockfile", "--offline"])
        .current_dir(consumer.path())
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );

    let problems = strings(&repo_meta::boundary::check(
        consumer.path(),
        "https://github.com/leinardi/rust-common",
        &[],
    ));
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(
        problems[0].starts_with("2 copies of jiff: 0.2.0 (path) via consumer -> jiff; 1."),
        "{problems:#?}"
    );
    assert!(
        problems[0].contains("(crates.io), rust-common's, via consumer -> clock -> jiff;"),
        "{problems:#?}"
    );
}
