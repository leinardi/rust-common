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

//! git commands that cannot reach the repository being committed (Go's `test/internal/gitenv`).
//!
//! Meta-tests also run inside a pre-commit hook, and git exports its own location to hooks:
//! `GIT_INDEX_FILE` always, `GIT_DIR` and others in some setups, absolute in a linked worktree. A
//! git command run in a scratch repository with that environment acts on the real one instead --
//! a `git add` there writes into the index of the commit being made. [`isolate`] strips those
//! variables and the user's own configuration, so a scratch repository is only ever itself.
//!
//! Extracted from swarm-scheduler-exporter/meta/src/gitenv.rs at c9e79afa8810 (the same as
//! awtrix-controller/meta/src/gitenv.rs at afcd9ca26324 and
//! swarm-rootless-docker/meta/src/gitenv.rs at 3bb0fd8a615b) and bitgauge/testenv/src/gitenv.rs
//! at 600fc3905200 (the public [`REPOSITORY_VARS`] and [`redirects`], and their test). A variable
//! name is matched on its bytes, so a name that is not UTF-8 but starts with `GIT_CONFIG` is
//! stripped too (bitgauge's lossy reading strips it; the other copies left it).

use std::env;
use std::ffi::{OsStr, OsString};
use std::process::Command;

/// The variables that point git at a repository other than the working directory's.
pub const REPOSITORY_VARS: [&str; 9] = [
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
];

/// Whether the variable `name` redirects git ([`REPOSITORY_VARS`]) or carries the user's
/// configuration (`GIT_CONFIG*`).
#[must_use]
pub fn redirects(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    REPOSITORY_VARS.iter().any(|var| var.as_bytes() == bytes) || bytes.starts_with(b"GIT_CONFIG")
}

/// `command` without the variables that redirect git and without the user's global and system
/// configuration (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`).
pub fn isolate(command: &mut Command) -> &mut Command {
    strip(command, env::vars_os().map(|(name, _)| name))
}

/// [`isolate`] for a process whose environment holds the variables `names`.
fn strip(command: &mut Command, names: impl IntoIterator<Item = OsString>) -> &mut Command {
    for name in names {
        if redirects(&name) {
            command.env_remove(&name);
        }
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt as _;
    use std::process::Command;

    use super::{isolate, redirects, strip};

    /// The redirecting variables are recognized, configuration included, and the isolated
    /// command carries the null configuration.
    #[test]
    fn isolate_strips_the_redirections() {
        for name in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_CEILING_DIRECTORIES",
            "GIT_CONFIG",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
        ] {
            assert!(redirects(OsStr::new(name)), "{name}");
        }
        for name in ["GIT_AUTHOR_NAME", "GIT_DIRX", "PATH", "HOME", "git_dir"] {
            assert!(!redirects(OsStr::new(name)), "{name}");
        }
        let mut command = Command::new("git");
        isolate(&mut command);
        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&("GIT_CONFIG_GLOBAL".to_owned(), Some("/dev/null".to_owned()))));
        assert!(envs.contains(&("GIT_CONFIG_NOSYSTEM".to_owned(), Some("1".to_owned()))));
    }

    /// The variables a hook exports are removed from the child's environment, and only those.
    #[test]
    fn the_hook_s_variables_are_removed() {
        let names = [
            OsString::from("GIT_INDEX_FILE"),
            OsString::from("GIT_DIR"),
            OsString::from("GIT_CONFIG_PARAMETERS"),
            OsString::from_vec(b"GIT_CONFIG_\xff".to_vec()),
            OsString::from("GIT_AUTHOR_NAME"),
            OsString::from("PATH"),
        ];
        let mut command = Command::new("git");
        strip(&mut command, names);
        let mut removed: Vec<OsString> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_owned())
            .collect();
        removed.sort();
        assert_eq!(
            removed,
            [
                OsString::from("GIT_CONFIG_PARAMETERS"),
                OsString::from_vec(b"GIT_CONFIG_\xff".to_vec()),
                OsString::from("GIT_DIR"),
                OsString::from("GIT_INDEX_FILE"),
            ]
        );
    }
}
