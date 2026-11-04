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

//! A run's envid, the label that namespaces everything it creates, and the resource names derived
//! from it. What differs between the suites is configuration, an [`EnvIds`] each defines once:
//!
//! - swarm-scheduler-exporter: 4 bytes, `swarm-scheduler-exporter.it.envid`, `sse-it`;
//! - swarm-device-access: 6 bytes (the Go suite's `TestMain`), `swarm-device-access-it.envid`,
//!   `sda-it`;
//! - bitgauge: 4 bytes, `io.github.leinardi.bitgauge.test.envid`, `bitgauge-test` (and
//!   [`EnvIds::cli_filter`] for its `label_filter`).
//!
//! Extracted from swarm-scheduler-exporter/testenv/src/envid.rs at c9e79afa8810 and
//! swarm-device-access/testenv/src/envid.rs at 8b4ab3a3e417 (the same code, four and six bytes),
//! with bitgauge/testenv/src/envid.rs at 600fc3905200 (the docker CLI's `label=` filter and the
//! bare `io::Result`). bitgauge's owner label helpers are [`crate::sweep_rules`]'s.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read as _};

use crate::Error;

/// How one suite names its runs: the random bytes of an envid, the Docker label key every resource
/// of a run carries it under, and the prefix of its resource names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvIds {
    /// Random bytes in an envid; it has twice as many lowercase hex characters.
    pub bytes: usize,
    /// The Docker label key applied to every resource the harness creates. Use it with
    /// [`EnvIds::label_filter`] to scope Docker API queries to one run, or on its own to find
    /// every run.
    pub label: &'static str,
    /// The first part of every resource name: `<prefix>-<envid>-<base>`.
    pub prefix: &'static str,
}

impl EnvIds {
    /// A fresh envid: `2 * bytes` lowercase hex characters from the kernel's random source, as
    /// Go's `crypto/rand` reads it.
    ///
    /// # Errors
    ///
    /// The random source cannot be read: `generate env ID: <error>`.
    pub fn generate(&self) -> Result<String, Error> {
        random_hex(self.bytes).map_err(|error| Error::new(format!("generate env ID: {error}")))
    }

    /// The name of the run's resource `base`: `<prefix>-<envid>-<base>`.
    #[must_use]
    pub fn resource_name(&self, env_id: &str, base: &str) -> String {
        format!("{}-{env_id}-{base}", self.prefix)
    }

    /// The `label` filter value that selects the run's resources, for the Engine API:
    /// `<label>=<envid>`.
    #[must_use]
    pub fn label_filter(&self, env_id: &str) -> String {
        format!("{}={env_id}", self.label)
    }

    /// The docker CLI's `--filter` value that selects the run's resources:
    /// `label=<label>=<envid>` (bitgauge's `label_filter`).
    #[must_use]
    pub fn cli_filter(&self, env_id: &str) -> String {
        format!("label={}", self.label_filter(env_id))
    }
}

/// `bytes` random bytes from `/dev/urandom`, as lowercase hex.
///
/// # Errors
///
/// `/dev/urandom` cannot be read.
pub fn random_hex(bytes: usize) -> io::Result<String> {
    let mut random = vec![0_u8; bytes];
    File::open("/dev/urandom").and_then(|mut source| source.read_exact(&mut random))?;
    Ok(random.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

#[cfg(test)]
mod tests {
    use super::EnvIds;

    const SSE: EnvIds = EnvIds {
        bytes: 4,
        label: "swarm-scheduler-exporter.it.envid",
        prefix: "sse-it",
    };
    const SDA: EnvIds = EnvIds {
        bytes: 6,
        label: "swarm-device-access-it.envid",
        prefix: "sda-it",
    };

    /// Go: `TestGenerateEnvID`, for each suite's length.
    #[test]
    fn an_envid_is_random_lowercase_hex() {
        for (ids, length) in [(SSE, 8), (SDA, 12)] {
            let first = ids.generate().unwrap();
            let second = ids.generate().unwrap();
            assert_eq!(first.len(), length, "{first}");
            assert!(
                first
                    .chars()
                    .all(|char| char.is_ascii_digit() || ('a'..='f').contains(&char)),
                "{first}"
            );
            assert_ne!(first, second, "two env IDs are the same");
        }
    }

    /// The names and filters each suite derives from an envid.
    #[test]
    fn names_and_filters() {
        assert_eq!(SSE.resource_name("1a2b3c4d", "net"), "sse-it-1a2b3c4d-net");
        assert_eq!(
            SDA.resource_name("0123456789ab", "x"),
            "sda-it-0123456789ab-x"
        );
        assert_eq!(
            SSE.label_filter("1a2b3c4d"),
            "swarm-scheduler-exporter.it.envid=1a2b3c4d"
        );
        let bitgauge = EnvIds {
            bytes: 4,
            label: "io.github.leinardi.bitgauge.test.envid",
            prefix: "bitgauge-test",
        };
        assert_eq!(
            bitgauge.resource_name("1a2b3c4d", "bitgauge"),
            "bitgauge-test-1a2b3c4d-bitgauge"
        );
        assert_eq!(
            bitgauge.cli_filter("1a2b3c4d"),
            "label=io.github.leinardi.bitgauge.test.envid=1a2b3c4d"
        );
    }
}
