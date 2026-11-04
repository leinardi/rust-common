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
//! tests live under `tests/` and run the checks of the `repo-meta` crate on this repository, each
//! with its parameters; this library holds only what is rust-common's own (the `feature-sets.toml`
//! reader and the feature-matrix runner behind `make rust-feature-matrix`,
//! `src/bin/feature-matrix.rs`) and the repository the tests read.

pub mod feature_sets;

use repo_meta::Repo;

/// This repository: the meta crate sits one level below its root.
#[must_use]
pub fn repo() -> Repo {
    Repo::new(testenv_core::repo_root!())
}
