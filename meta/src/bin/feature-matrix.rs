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

//! `make rust-feature-matrix`: builds and tests each crate `feature-sets.toml` lists under each of
//! its sets, one package at a time, and stops at the first failure. The commands come from
//! [`meta::feature_sets::plan`]; the cargo that runs them is the one that started this binary
//! (`CARGO`), so the toolchain is the same.

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::{Command, ExitCode};

use meta::feature_sets::{FILE, plan};
use meta::repo;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(io::stderr(), "feature-matrix: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<ExitCode> {
    let root = repo::root();
    let mut out = io::stdout();
    let mut err = io::stderr();

    let commands = match plan(&root) {
        Ok(commands) => commands,
        Err(problems) => {
            for problem in problems {
                writeln!(err, "feature-matrix: {problem}")?;
            }
            return Ok(ExitCode::FAILURE);
        }
    };
    if commands.is_empty() {
        writeln!(
            out,
            "feature-matrix: {FILE} lists no crate, so there is nothing to build or test."
        )?;
        return Ok(ExitCode::SUCCESS);
    }

    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    for args in &commands {
        let line = format!("cargo {}", args.join(" "));
        writeln!(out, "==> {line}")?;
        out.flush()?;
        let status = Command::new(&cargo)
            .args(args)
            .current_dir(&root)
            .status()?;
        if !status.success() {
            writeln!(err, "feature-matrix: {line} failed ({status})")?;
            return Ok(ExitCode::FAILURE);
        }
    }
    writeln!(
        out,
        "feature-matrix: all {} commands passed.",
        commands.len()
    )?;
    Ok(ExitCode::SUCCESS)
}
