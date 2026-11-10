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

//! Parameterised repository checks for the meta-tests of the projects that share this workspace:
//! a project's meta crate calls them from thin tests, passing its own paths and pins, and
//! keeps local only what is its own.
//!
//! Every check takes the contents it judges (or a root it reads through) and returns the
//! [`Problem`]s it finds, empty on a passing repository, and never panics on what it reads. So a
//! meta-test asserts the list is empty, and its negative control runs the same check on a copy
//! with a planted violation and asserts what comes back. Where the consumers legitimately differ
//! (an exact Rust release or only the line in `rust-version`), the difference is a parameter; for
//! the same parameters each check is as strict as the strictest consumer's copy.
//!
//! - [`Repo`]: the root, `read`, `tracked`, `tracked_or_new` and `files_under`.
//! - [`scratch`]: scratch git repositories, for the negative controls that plant a violation.
//! - [`dockerfile`]: the Dockerfile reader and the base-pin rules; [`image`]: the image pins
//!   (the Dockerfiles a repository holds, the frontend and the base pins).
//! - [`yaml`]: workflows, actions and the pre-commit configuration.
//! - [`workspace`]: every member opts into the workspace lints.
//! - [`toolchain`]: one value per pin across files ([`toolchain::one_value`]), with presets
//!   for the Rust release in `rust-toolchain.toml`, Cargo's `rust-version` and a Dockerfile, and
//!   [`toolchain::make_value`].
//! - [`ci`]: `ci-result` needs every gate, `pre-commit-all` skips only hooks a needed job runs.
//! - [`git_pins`]: the `tag` and `version` pins of a consumer's dependencies on this workspace's
//!   git repository agree with the release and with each other.
//! - [`vacuous`]: no gate carries a flag that makes it pass on nothing.
//!
//! The crate needs no network, no Docker and no tool beyond git.

pub mod ci;
pub mod dockerfile;
pub mod git_pins;
pub mod image;
mod problem;
mod repo;
pub mod scratch;
pub mod toolchain;
pub mod vacuous;
pub mod workspace;
pub mod yaml;

pub use problem::{Problem, report, strings};
pub use repo::Repo;

use regex::Regex;

/// Compiles a pattern a test writes, for the checks that take one (a pin's source, an allowed
/// fragment's file).
///
/// # Panics
///
/// The pattern does not compile, naming it: a bug in the test, which its first run reaches.
#[must_use]
pub fn pattern(text: &str) -> Regex {
    Regex::new(text).unwrap_or_else(|error| panic!("invalid pattern {text}: {error}"))
}

/// Compiles a pattern this crate writes itself.
fn regex(text: &str) -> Regex {
    pattern(text)
}
