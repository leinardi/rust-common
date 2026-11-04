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

//! `make rust-feature-matrix`: for each crate `feature-sets.toml` lists and each of its sets,
//! clippy over every target with warnings as errors, then the tests, one package at a time, stopping
//! at the first failure ([`meta::feature_sets::run`]). The cargo that runs them is the one that
//! started this binary (`CARGO`), so the toolchain is the same.

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use meta::feature_sets::run;
use meta::repo;

fn main() -> ExitCode {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    match run(
        &repo::root(),
        &cargo,
        &mut io::stdout(),
        &mut io::stderr(),
        true,
    ) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            let _ = writeln!(io::stderr(), "feature-matrix: {error}");
            ExitCode::FAILURE
        }
    }
}
