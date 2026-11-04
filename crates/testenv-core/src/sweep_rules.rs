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

//! The sweep's decision rules, the owner of a run and the labels that record it, free of the
//! Docker SDK, so their tests run in a plain `cargo test` with no daemon. The label keys and the
//! stale-after variable are each suite's own: a [`Labels`] and the variable's name.
//!
//! - swarm-scheduler-exporter: `swarm-scheduler-exporter.it.` and `owner`, `started`, `pidns`;
//!   `SSE_IT_STALE_AFTER`;
//! - swarm-device-access: `swarm-device-access-it.` and the same three; `SDA_IT_STALE_AFTER`.
//!
//! Each with [`DEFAULT_STALE_AFTER`], an hour. bitgauge's sweep is another design (a keep marker,
//! no PID namespace, jiff's friendly durations, six hours) and stays its own; it shares the
//! [`Owner`] label helpers.
//!
//! Extracted from swarm-scheduler-exporter/testenv/src/sweep_rules.rs at c9e79afa8810 and
//! swarm-device-access/testenv/src/sweep_rules.rs at 8b4ab3a3e417 (the same rules, other labels).
//! Extracted from swarm-scheduler-exporter/testenv/src/cluster.rs at c9e79afa8810 (`labels`),
//! swarm-device-access/testenv/src/host.rs at 8b4ab3a3e417 (`run_labels`, `record`,
//! `cleanup_command`) and swarm-scheduler-exporter/testenv/src/dind.rs at c9e79afa8810 (`record`,
//! `cleanup_command`), for [`Labels`] and [`Resources`].

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::time::Duration;

use go_compat::{duration, quote};
use jiff::Timestamp;

use crate::Error;

/// The age below which no other environment is ever removed, unless the suite's variable says
/// otherwise ([`stale_after_from_env`]).
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(3600);

const NANOS_PER_MINUTE: i64 = 60_000_000_000;

/// The label keys a suite records a run with, beside its envid label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Labels {
    /// The envid ([`crate::envid::EnvIds::label`]).
    pub env_id: &'static str,
    /// `<hostname>:<pid>` of the test process that created the resource.
    pub owner: &'static str,
    /// When the environment came up, in RFC 3339.
    pub started: &'static str,
    /// The PID namespace of the owner (`pid:[4026531836]`): a PID means something only inside
    /// its namespace, and a test in a container that shares the host's hostname
    /// (`--network host`) has its own.
    pub pid_namespace: &'static str,
}

impl Labels {
    /// The labels every resource of the run carries: the envid, which the cleanup and
    /// `make sweep-test-leaks` select by, and the owner, start time and PID namespace the sweep
    /// of a later run decides by.
    #[must_use]
    pub fn run_labels(
        &self,
        env_id: &str,
        owner: &Owner,
        started: Timestamp,
    ) -> HashMap<String, String> {
        HashMap::from([
            (self.env_id.to_owned(), env_id.to_owned()),
            (self.owner.to_owned(), owner.to_string()),
            (
                self.started.to_owned(),
                started.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
            ),
            (self.pid_namespace.to_owned(), owner.pid_namespace.clone()),
        ])
    }

    /// Adds one resource's labels to the environment they name; a resource without the envid
    /// label, or with an empty one, names none.
    pub fn record(
        &self,
        by_id: &mut BTreeMap<String, Found>,
        labels: Option<&HashMap<String, String>>,
    ) {
        let Some(labels) = labels else {
            return;
        };
        let Some(env_id) = labels.get(self.env_id).filter(|id| !id.is_empty()) else {
            return;
        };
        let found = by_id.entry(env_id.clone()).or_insert_with(|| Found {
            id: env_id.clone(),
            ..Found::default()
        });
        // A resource may carry the envid alone (a helper container, a Go suite's container); the
        // owner and start time come from whichever resource has them.
        for (field, key) in [
            (&mut found.owner, self.owner),
            (&mut found.started, self.started),
            (&mut found.pid_namespace, self.pid_namespace),
        ] {
            if field.is_empty()
                && let Some(value) = labels.get(key)
            {
                value.clone_into(field);
            }
        }
    }
}

/// What a run leaves on the host daemon, beside its containers: what the cleanup and the sweep
/// remove, in order (containers, networks, images).
///
/// swarm-scheduler-exporter's runs leave containers with their volumes and networks
/// (`remove_volumes` and `networks`); swarm-device-access's leave containers and images
/// (`images`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    /// Whether a container's anonymous volumes go with it (`docker rm -v`).
    pub remove_volumes: bool,
    /// Whether the run's networks are removed.
    pub networks: bool,
    /// Whether the run's images are removed.
    pub images: bool,
}

impl Resources {
    /// A shell command that removes every resource of the run that `filter` selects, a docker
    /// CLI `--filter` value ([`crate::envid::EnvIds::cli_filter`]).
    #[must_use]
    pub fn cleanup_command(&self, filter: &str) -> String {
        let volumes = if self.remove_volumes { "v" } else { "" };
        let mut command =
            format!("docker ps -aq --filter {filter} | xargs -r docker rm -f{volumes}");
        if self.networks {
            let _ = write!(
                command,
                "; docker network ls -q --filter {filter} | xargs -r docker network rm"
            );
        }
        if self.images {
            let _ = write!(
                command,
                "; docker image ls -q --filter {filter} | xargs -r docker rmi -f"
            );
        }
        command
    }
}

/// The process that created an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    /// The hostname.
    pub host: String,
    /// The PID, positive.
    pub pid: i64,
    /// The namespace the PID lives in, empty where it cannot be read.
    pub pid_namespace: String,
}

impl std::fmt::Display for Owner {
    /// The owner label's value, `<hostname>:<pid>`; the namespace has a label of its own.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.host, self.pid)
    }
}

/// This process on this host.
///
/// # Errors
///
/// The hostname cannot be read.
pub fn current_owner() -> Result<Owner, Error> {
    Ok(Owner {
        host: hostname()?,
        pid: i64::from(std::process::id()),
        pid_namespace: current_pid_namespace(),
    })
}

/// Go's `os.Hostname` on Linux: the kernel's hostname, as `/proc` reports it.
///
/// # Errors
///
/// `/proc/sys/kernel/hostname` cannot be read: `hostname: <error>`.
pub fn hostname() -> Result<String, Error> {
    let raw = fs::read_to_string("/proc/sys/kernel/hostname")
        .map_err(|error| Error::new(format!("hostname: {error}")))?;
    Ok(raw.strip_suffix('\n').unwrap_or(&raw).to_owned())
}

/// This process's PID namespace, or "" where `/proc` does not say. An empty namespace never
/// matches, so an environment from such a process is never swept by PID.
#[must_use]
pub fn current_pid_namespace() -> String {
    fs::read_link("/proc/self/ns/pid")
        .map(|target| target.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads an owner label value. The PID is after the last colon, so a hostname may hold colons
/// of its own.
///
/// # Errors
///
/// No colon, nothing before it, or a PID that is not a positive integer (Go's `strconv.Atoi`):
/// `owner is not <hostname>:<pid>: <value, Go-quoted>`.
pub fn parse_owner(value: &str) -> Result<Owner, Error> {
    let bad = || {
        Error::new(format!(
            "owner is not <hostname>:<pid>: {}",
            quote::quote(value)
        ))
    };
    let colon = value
        .rfind(':')
        .filter(|&colon| colon > 0)
        .ok_or_else(bad)?;
    let pid: i64 = value[colon + 1..].parse().map_err(|_| bad())?;
    if pid <= 0 {
        return Err(bad());
    }
    Ok(Owner {
        host: value[..colon].to_owned(),
        pid,
        pid_namespace: String::new(),
    })
}

/// One environment as its labels describe it. The owner and started labels are taken from
/// whichever of its resources carries them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// The envid.
    pub id: String,
    /// The owner label's value.
    pub owner: String,
    /// The started label's value.
    pub started: String,
    /// The PID namespace label's value.
    pub pid_namespace: String,
}

/// The sweep's decision about one environment, with the reason a reader is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The environment decided on.
    pub env: Found,
    /// Whether the sweep removes it.
    pub remove: bool,
    /// Why.
    pub reason: String,
}

/// Decides which environments are abandoned. Another envid is removed only when all of these
/// hold: it is older than `stale_after`, and its owner is provably dead -- on this host, in this
/// PID namespace, and its PID does not exist. A foreign host is never assumed dead, and a label
/// that cannot be read proves nothing: such an environment is left for `make sweep-test-leaks`.
pub struct SweepRules {
    /// The time the ages are measured at.
    pub now: Timestamp,
    /// The age below which nothing is removed.
    pub stale_after: Duration,
    /// This host.
    pub host: String,
    /// This process's PID namespace; empty matches nothing.
    pub pid_namespace: String,
    /// The current envid, which a sweep never touches; `down` owns it.
    pub self_id: String,
    /// Whether a PID exists on this host.
    pub alive: Box<dyn Fn(i64) -> bool + Send + Sync>,
}

impl SweepRules {
    /// Applies the rules to one environment.
    #[must_use]
    pub fn decide(&self, env: Found) -> Verdict {
        let keep = |env: Found, reason: String| Verdict {
            env,
            remove: false,
            reason,
        };

        if env.id == self.self_id {
            return keep(env, "the current environment".to_owned());
        }

        // jiff reads RFC 3339 as Go's time.Parse does for every value the harness writes; it is
        // laxer about case and the separator, which no label written here uses.
        let Ok(started) = env.started.parse::<Timestamp>() else {
            return keep(env, "its started label is unreadable".to_owned());
        };

        let age = saturating_nanos(self.now.duration_since(started).as_nanos());
        let stale_after = i64::try_from(self.stale_after.as_nanos()).unwrap_or(i64::MAX);
        if age < stale_after {
            return keep(
                env,
                format!(
                    "started {} ago, under {}",
                    duration::format(round(age, NANOS_PER_MINUTE)),
                    duration::format(stale_after)
                ),
            );
        }

        let Ok(owner) = parse_owner(&env.owner) else {
            return keep(env, "its owner label is unreadable".to_owned());
        };

        if owner.host != self.host {
            return keep(env, format!("owned by another host, {}", owner.host));
        }

        if self.pid_namespace.is_empty() || env.pid_namespace != self.pid_namespace {
            return keep(
                env,
                "owned by a process in another or unknown PID namespace".to_owned(),
            );
        }

        if (self.alive)(owner.pid) {
            return keep(
                env,
                format!("its owner, PID {}, is still running", owner.pid),
            );
        }

        Verdict {
            env,
            remove: true,
            reason: format!("stale, and its owner PID {} is gone", owner.pid),
        }
    }
}

/// Reads the suite's stale-after variable `name` (`SSE_IT_STALE_AFTER`, `SDA_IT_STALE_AFTER`)
/// through `getenv`, defaulting to `default` ([`DEFAULT_STALE_AFTER`]) when it is unset or empty.
///
/// # Errors
///
/// A value that is not a positive Go duration, rather than a default: a typo must not shorten
/// the window. `bad stale-after window: <name>=<value, Go-quoted> is not a positive duration`.
pub fn stale_after_from_env(
    name: &str,
    default: Duration,
    getenv: impl Fn(&str) -> Option<String>,
) -> Result<Duration, Error> {
    let raw = getenv(name).unwrap_or_default();
    if raw.is_empty() {
        return Ok(default);
    }
    match duration::parse(&raw) {
        Ok(nanos) if nanos > 0 => Ok(Duration::from_nanos(nanos.unsigned_abs())),
        Ok(_) | Err(_) => Err(Error::new(format!(
            "bad stale-after window: {name}={} is not a positive duration",
            quote::quote(&raw)
        ))),
    }
}

fn saturating_nanos(nanos: i128) -> i64 {
    i64::try_from(nanos).unwrap_or(if nanos < 0 { i64::MIN } else { i64::MAX })
}

/// Go's `Duration.Round`: to the nearest multiple of `multiple`, halfway values away from zero,
/// saturating at the ends of the range.
fn round(nanos: i64, multiple: i64) -> i64 {
    if multiple <= 0 {
        return nanos;
    }
    let less_than_half = |remainder: i64| remainder.unsigned_abs() * 2 < multiple.unsigned_abs();
    let remainder = nanos % multiple;
    if nanos < 0 {
        let remainder = -remainder;
        if less_than_half(remainder) {
            return nanos + remainder;
        }
        return nanos.checked_sub(multiple - remainder).unwrap_or(i64::MIN);
    }
    if less_than_half(remainder) {
        return nanos - remainder;
    }
    nanos.checked_add(multiple - remainder).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
