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

//! The build's version stamps, Go's `-ldflags -X main.version=…`: the build sets
//! `<PREFIX>_VERSION`, `<PREFIX>_COMMIT` and `<PREFIX>_DATE` in the environment (make-common's
//! `RUST_STAMP_PREFIX`, the image's build arguments) and the binary reads them at compile time.
//!
//! The reads are `option_env!`, which reads the environment of the crate being compiled, so they
//! must expand in the program's crate, not in this one: [`buildinfo!`](crate::buildinfo!) is a
//! macro for that reason, and [`BuildInfo::resolve`] only applies the defaults.
//!
//! Extracted from swarm-scheduler-exporter/src/version.rs at c9e79afa8810 (with the same rules as
//! swarm-device-access/src/version.rs at 8b4ab3a3e417, awtrix-controller/src/version.rs at
//! afcd9ca26324 and swarm-rootless-docker/src/version.rs at 3bb0fd8a615b: an empty stamp is kept)
//! and bitgauge/src/buildinfo.rs at 600fc3905200 (an empty stamp counts as unstamped).

/// What an empty stamp, a variable set to the empty string, means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyStamp {
    /// The empty string is the value, as `-X main.version=` set it in Go.
    Kept,
    /// The empty string counts as no stamp, and the default applies.
    Unstamped,
}

/// The values of the stamps a build did not set, and the empty-stamp rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Defaults {
    /// The version of an unstamped build (`"dev"` in every project so far).
    pub version: &'static str,
    /// The commit of an unstamped build (`"none"`, or `"unknown"`).
    pub commit: &'static str,
    /// The build date of an unstamped build (`"unknown"`).
    pub date: &'static str,
    /// What an empty stamp means.
    pub empty: EmptyStamp,
}

/// The build's version, commit and date, as stamped or defaulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    /// The release version.
    pub version: &'static str,
    /// The source commit.
    pub commit: &'static str,
    /// The build date.
    pub date: &'static str,
}

impl BuildInfo {
    /// The stamps `version`, `commit` and `date` (each `None` when the build did not set it), with
    /// `defaults` in place of a missing one, and of an empty one under [`EmptyStamp::Unstamped`].
    /// [`buildinfo!`](crate::buildinfo!) calls it with the program's own `option_env!` reads; a
    /// test calls it directly.
    #[must_use]
    pub const fn resolve(
        version: Option<&'static str>,
        commit: Option<&'static str>,
        date: Option<&'static str>,
        defaults: Defaults,
    ) -> Self {
        Self {
            version: stamp(version, defaults.version, defaults.empty),
            commit: stamp(commit, defaults.commit, defaults.empty),
            date: stamp(date, defaults.date, defaults.empty),
        }
    }
}

/// The stamp, or `default` when it is missing, or empty under [`EmptyStamp::Unstamped`].
const fn stamp(
    value: Option<&'static str>,
    default: &'static str,
    empty: EmptyStamp,
) -> &'static str {
    match (value, empty) {
        (None, _) => default,
        (Some(value), EmptyStamp::Unstamped) if value.is_empty() => default,
        (Some(value), EmptyStamp::Kept | EmptyStamp::Unstamped) => value,
    }
}

/// The program's [`BuildInfo`](crate::BuildInfo), read from `<PREFIX>_VERSION`, `<PREFIX>_COMMIT`
/// and `<PREFIX>_DATE` at compile time with the given [`Defaults`](crate::Defaults). `PREFIX` is a
/// string literal (`concat!` builds the names); the defaults are any constant expression.
///
/// The `option_env!` reads expand where the macro is called, so they see the calling crate's
/// build environment, and the result is a constant:
///
/// ```
/// use service_kit::{BuildInfo, Defaults, EmptyStamp, buildinfo};
///
/// const BUILD: BuildInfo = buildinfo!(
///     "SERVICE_KIT_DOC_EXAMPLE",
///     Defaults { version: "dev", commit: "none", date: "unknown", empty: EmptyStamp::Kept },
/// );
///
/// // Nothing sets SERVICE_KIT_DOC_EXAMPLE_*: the defaults.
/// assert_eq!(BUILD, BuildInfo { version: "dev", commit: "none", date: "unknown" });
/// ```
#[macro_export]
macro_rules! buildinfo {
    ($prefix:literal, $defaults:expr $(,)?) => {
        $crate::BuildInfo::resolve(
            ::core::option_env!(::core::concat!($prefix, "_VERSION")),
            ::core::option_env!(::core::concat!($prefix, "_COMMIT")),
            ::core::option_env!(::core::concat!($prefix, "_DATE")),
            $defaults,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const GO: Defaults = Defaults {
        version: "dev",
        commit: "none",
        date: "unknown",
        empty: EmptyStamp::Kept,
    };

    const BITGAUGE: Defaults = Defaults {
        version: "dev",
        commit: "unknown",
        date: "unknown",
        empty: EmptyStamp::Unstamped,
    };

    fn build(version: &'static str, commit: &'static str, date: &'static str) -> BuildInfo {
        BuildInfo {
            version,
            commit,
            date,
        }
    }

    /// swarm-scheduler-exporter's `unset_stamps_fall_back_to_go_defaults`: an unset stamp takes
    /// the default, an empty one stays empty.
    #[test]
    fn kept_empty_stamps_stay_empty() {
        assert_eq!(
            BuildInfo::resolve(None, None, None, GO),
            build("dev", "none", "unknown")
        );
        assert_eq!(
            BuildInfo::resolve(Some(""), Some(""), Some(""), GO),
            build("", "", "")
        );
        assert_eq!(
            BuildInfo::resolve(
                Some("v1.2.3"),
                Some("abc"),
                Some("2026-01-02T03:04:05Z"),
                GO
            ),
            build("v1.2.3", "abc", "2026-01-02T03:04:05Z")
        );
    }

    /// bitgauge's `TestResolve`: an empty stamp counts as unstamped.
    #[test]
    fn unstamped_empty_stamps_take_the_defaults() {
        for (name, (version, commit, date), want) in [
            (
                "nothing stamped",
                (None, None, None),
                build("dev", "unknown", "unknown"),
            ),
            (
                "empty stamps",
                (Some(""), Some(""), Some("")),
                build("dev", "unknown", "unknown"),
            ),
            (
                "everything stamped",
                (
                    Some("v0.1.0"),
                    Some("a1b2c3d4e5f6"),
                    Some("2026-10-12T08:00:00Z"),
                ),
                build("v0.1.0", "a1b2c3d4e5f6", "2026-10-12T08:00:00Z"),
            ),
            (
                "only the version stamped",
                (Some("v0.1.0-3-gabcdef0-dirty"), None, None),
                build("v0.1.0-3-gabcdef0-dirty", "unknown", "unknown"),
            ),
            (
                "commit and date stamped, version empty",
                (Some(""), Some("abc"), Some("2026-10-12T08:00:00Z")),
                build("dev", "abc", "2026-10-12T08:00:00Z"),
            ),
        ] {
            assert_eq!(
                BuildInfo::resolve(version, commit, date, BITGAUGE),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn resolve_is_a_constant() {
        const BUILD: BuildInfo = BuildInfo::resolve(Some("v1"), None, Some(""), GO);
        assert_eq!(BUILD, build("v1", "none", ""));
    }
}
