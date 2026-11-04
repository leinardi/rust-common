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

//! Repository meta-tests: checks on the repository itself rather than on the library crates. The
//! tests live under `tests/`; this library holds what they share (the repository's files, the
//! Markdown link reader, the `feature-sets.toml` reader), and `src/bin/feature-matrix.rs` is the
//! runner behind `make rust-feature-matrix`.
//!
//! `links` and `repo` are this crate's own copies until the `repo-meta` crate replaces them.

pub mod feature_sets;
pub mod links;
pub mod repo;
