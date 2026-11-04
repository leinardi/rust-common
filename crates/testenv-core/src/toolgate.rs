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

//! The external tools that behavioural tests drive (Go's `test/internal/toolgate`).
//!
//! A behavioural test that quietly passes when its tool is missing passes in exactly the place it
//! is meant to guard: CI. So a missing tool ends the test early, with a note, on a developer
//! machine, and fails it wherever the suite's variable is `1`, which CI sets. The tools are a
//! closed list: widening it is a decision, not a side effect of a new test.
//!
//! What differs between the projects is configuration, a [`Gate`] each defines once:
//!
//! - awtrix-controller: `AWTRIX_CONTROLLER_REQUIRE_TOOLS`, `droast` and `shellcheck`,
//!   [`Locate::Path`], [`Note::Stderr`];
//! - swarm-rootless-docker: `SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS`, the same tools, locate and note;
//! - bitgauge: `BITGAUGE_REQUIRE_TOOLS`, `eslint` and `fallow`, [`Locate::Ancestors`] with `dir`
//!   `web/node_modules/.bin` and `hint` `run npm ci in web/`, [`Note::Captured`].
//!
//! Extracted from bitgauge/testenv/src/toolgate.rs at 600fc3905200 (the design: a closed tool set,
//! [`Required`], the testable [`Gate::lookup_in`], the ancestor search, [`crate::repo_root!`]) and
//! awtrix-controller/meta/src/toolgate.rs at afcd9ca26324 (the same as
//! swarm-rootless-docker/meta/src/toolgate.rs at 3bb0fd8a615b but for the variable: the `PATH`
//! search of an executable file and its messages).

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// One project's tool gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    /// The variable that turns a missing tool from an early, passing return into a failure when
    /// it is exactly `1`.
    pub require_env: &'static str,
    /// The closed list of tools a test may be gated on, by program name.
    pub tools: &'static [&'static str],
    /// Where a tool is looked for.
    pub locate: Locate,
    /// Where the note about a missing tool goes.
    pub note: Note,
}

/// Where a [`Gate`] looks a tool up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locate {
    /// An executable file named as the tool in a `PATH` directory, the first one winning
    /// (awtrix-controller's and swarm-rootless-docker's meta-tests).
    Path,
    /// `<dir>/<tool>` in the nearest ancestor of the start directory that has one, whatever its
    /// mode (bitgauge's `web/node_modules/.bin`); a missing one says `<dir>/<tool>: not installed;
    /// <hint>`.
    Ancestors {
        /// The directory below an ancestor, such as `web/node_modules/.bin`.
        dir: &'static str,
        /// What installs the tools, such as `run npm ci in web/`.
        hint: &'static str,
    },
}

/// Where a [`Gate`] writes the note about a missing tool that is not required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// Straight to the process's standard error, past libtest's capture, so the note shows on a
    /// passing run (awtrix-controller, swarm-rootless-docker):
    /// `<tool> not found on PATH: this check did not run; set <VAR>=1 to make it a failure`.
    Stderr,
    /// Through `eprintln!`, which libtest captures, so it shows only for a failing test or with
    /// `--nocapture` (bitgauge): `note: <tool> not found (<reason>); set <VAR>=1 to make this a
    /// failure`.
    Captured,
}

/// A required tool that is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Required {
    /// The tool.
    pub tool: String,
    /// The variable that made it required.
    pub require_env: &'static str,
    /// Why it was not found, or `None` for a tool not found on `PATH`.
    pub reason: Option<String>,
}

impl fmt::Display for Required {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            None => write!(
                formatter,
                "{} is required ({}=1) but was not found on PATH",
                self.tool, self.require_env
            ),
            Some(reason) => write!(
                formatter,
                "{} is required ({}=1) but was not found: {reason}",
                self.tool, self.require_env
            ),
        }
    }
}

impl std::error::Error for Required {}

impl Gate {
    /// The path of `tool`, looked up from the working directory with the gate's variable read
    /// from the environment. `None`, after the note, when it is missing and the test should
    /// return early.
    ///
    /// # Panics
    ///
    /// `tool` is not one of [`Gate::tools`], or it is missing while [`Gate::require_env`] is `1`:
    /// the test fails, with [`Required`]'s message.
    #[must_use]
    pub fn lookup(&self, tool: &str) -> Option<PathBuf> {
        let start = env::current_dir().unwrap_or_default();
        let required = env::var_os(self.require_env).is_some_and(|value| value == "1");
        self.lookup_in(tool, &start, env::var_os("PATH"), required)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// [`Gate::lookup`] from the directory `start` (for [`Locate::Ancestors`]), with `path` for
    /// `PATH` (for [`Locate::Path`]) and `required` for the gate's variable.
    ///
    /// # Panics
    ///
    /// `tool` is not one of [`Gate::tools`]:
    /// `toolgate: "<tool>" is not one of the gated tools [...]`.
    ///
    /// # Errors
    ///
    /// [`Required`] when the tool is missing and `required` is set.
    pub fn lookup_in(
        &self,
        tool: &str,
        start: &Path,
        path: Option<OsString>,
        required: bool,
    ) -> Result<Option<PathBuf>, Required> {
        match self.resolve(tool, start, path, required)? {
            Resolved::Found(found) => Ok(Some(found)),
            Resolved::Missing { note, text } => {
                match note {
                    Note::Stderr => {
                        // Nowhere left to report a failed write to standard error.
                        let _ = writeln!(io::stderr(), "{text}");
                    }
                    Note::Captured => captured_note(&text),
                }
                Ok(None)
            }
        }
    }

    /// [`Gate::lookup_in`] without the note written: the tool found, or the note a missing one
    /// gets and where it goes.
    fn resolve(
        &self,
        tool: &str,
        start: &Path,
        path: Option<OsString>,
        required: bool,
    ) -> Result<Resolved, Required> {
        assert!(
            self.tools.contains(&tool),
            "toolgate: {tool:?} is not one of the gated tools {:?}",
            self.tools
        );
        let reason = match self.find(tool, start, path) {
            Ok(found) => return Ok(Resolved::Found(found)),
            Err(reason) => reason,
        };
        if required {
            return Err(Required {
                tool: tool.to_owned(),
                require_env: self.require_env,
                reason,
            });
        }
        let text = match &reason {
            None => format!(
                "{tool} not found on PATH: this check did not run; set {}=1 to make it a failure",
                self.require_env
            ),
            Some(reason) => format!(
                "note: {tool} not found ({reason}); set {}=1 to make this a failure",
                self.require_env
            ),
        };
        Ok(Resolved::Missing {
            note: self.note,
            text,
        })
    }

    /// The tool, or why it is missing (`None` on `PATH`, which needs no reason).
    fn find(
        &self,
        tool: &str,
        start: &Path,
        path: Option<OsString>,
    ) -> Result<PathBuf, Option<String>> {
        match self.locate {
            Locate::Path => path
                .and_then(|paths| {
                    env::split_paths(&paths)
                        .map(|directory| directory.join(tool))
                        .find(|candidate| is_executable(candidate))
                })
                .ok_or(None),
            Locate::Ancestors { dir, hint } => {
                for ancestor in start.ancestors() {
                    let candidate = ancestor.join(dir).join(tool);
                    match fs::metadata(&candidate) {
                        Ok(_) => return Ok(candidate),
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(Some(format!("stat {}: {error}", candidate.display())));
                        }
                    }
                }
                Err(Some(format!("{dir}/{tool}: not installed; {hint}")))
            }
        }
    }
}

/// What [`Gate::resolve`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolved {
    /// The tool's path.
    Found(PathBuf),
    /// The tool is missing and not required: the note, and where it goes.
    Missing { note: Note, text: String },
}

#[expect(
    clippy::print_stderr,
    reason = "libtest captures eprintln!, so the note shows only for a failing test, as \
              bitgauge's toolgate prints it"
)]
fn captured_note(note: &str) {
    eprintln!("{note}");
}

fn is_executable(candidate: &Path) -> bool {
    candidate
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// The repository root of the crate that expands it: the parent of its `CARGO_MANIFEST_DIR`, for
/// a crate that sits one level below the root (`meta`, `testenv`). `env!` expands in the caller,
/// so it names the caller's manifest, never this crate's.
#[macro_export]
macro_rules! repo_root {
    () => {
        ::std::path::Path::new(::core::env!("CARGO_MANIFEST_DIR")).join("..")
    };
}

#[cfg(test)]
mod tests;
