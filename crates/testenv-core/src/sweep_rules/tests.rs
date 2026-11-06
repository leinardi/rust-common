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

//! The sweep rules' tests: the owner label, every verdict, the stale-after window, and each
//! suite's labels and cleanup command.
//!
//! Extracted from swarm-scheduler-exporter/testenv/src/sweep_rules/tests.rs at c9e79afa8810 (the
//! same as swarm-device-access/testenv/src/sweep_rules/tests.rs at 8b4ab3a3e417).

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use super::{
    DEFAULT_STALE_AFTER, Found, Labels, Owner, Resources, SweepRules, parse_owner, round,
    stale_after_from_env,
};

/// swarm-scheduler-exporter's variable, as a sample.
const ENV_STALE_AFTER: &str = "SSE_IT_STALE_AFTER";

const SSE_LABELS: Labels = Labels {
    env_id: "swarm-scheduler-exporter.it.envid",
    owner: "swarm-scheduler-exporter.it.owner",
    started: "swarm-scheduler-exporter.it.started",
    pid_namespace: "swarm-scheduler-exporter.it.pidns",
};

fn timestamp(text: &str) -> Timestamp {
    text.parse()
        .unwrap_or_else(|error| panic!("timestamp {text}: {error}"))
}

/// The time as the harness writes it in the started label: RFC 3339, UTC, whole seconds.
fn rfc3339(at: Timestamp) -> String {
    at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Go: `TestParseOwner`.
#[test]
fn parse_owner_takes_the_pid_after_the_last_colon() {
    let owner = parse_owner("runner:fe80::1:4242").unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        (owner.host.as_str(), owner.pid),
        ("runner:fe80::1", 4242),
        "parse_owner kept {owner:?}; want the PID after the last colon"
    );

    let formatted = Owner {
        host: "ci".to_owned(),
        pid: 7,
        pid_namespace: "pid:[1]".to_owned(),
    }
    .to_string();
    assert_eq!(
        formatted, "ci:7",
        "an owner does not format as <hostname>:<pid>"
    );

    for bad in ["", "host", ":12", "host:", "host:x", "host:0", "host:-3"] {
        assert!(parse_owner(bad).is_err(), "parse_owner({bad:?}) succeeded");
    }
    // The value is quoted as Go's %q quotes it.
    assert_eq!(
        parse_owner("host\t:x").unwrap_err().to_string(),
        r#"owner is not <hostname>:<pid>: "host\t:x""#
    );
}

fn found(id: &str, owner: &str, started: &str, pid_namespace: &str) -> Found {
    Found {
        id: id.to_owned(),
        owner: owner.to_owned(),
        started: started.to_owned(),
        pid_namespace: pid_namespace.to_owned(),
    }
}

fn rules(now: Timestamp, pid_namespace: &str) -> SweepRules {
    SweepRules {
        now,
        stale_after: Duration::from_secs(3600),
        host: "here".to_owned(),
        pid_namespace: pid_namespace.to_owned(),
        self_id: "self".to_owned(),
        alive: Box::new(|pid| pid == 100),
    }
}

/// Go: `TestSweepRulesDecide`.
#[test]
fn sweep_rules_decide() {
    let now = timestamp("2026-10-11T12:00:00Z");
    let old = rfc3339(now - SignedDuration::from_hours(2));
    let recent = rfc3339(now - SignedDuration::from_mins(10));

    let verdict = rules(now, "").decide(found("a", "here:200", &old, ""));
    assert!(
        !verdict.remove,
        "a sweeper that cannot read its own PID namespace removed an environment"
    );

    let rules = rules(now, "pid:[1]");
    let cases = [
        (
            "the current environment",
            found("self", "here:200", &old, "pid:[1]"),
            false,
            "current",
        ),
        (
            "recent, owner gone",
            found("a", "here:200", &recent, "pid:[1]"),
            false,
            "started 600s ago, under 3600s",
        ),
        (
            "stale, owner running",
            found("a", "here:100", &old, "pid:[1]"),
            false,
            "still running",
        ),
        (
            "stale, another host",
            found("a", "there:200", &old, ""),
            false,
            "another host",
        ),
        (
            "stale, no owner label",
            found("a", "", &old, ""),
            false,
            "owner label",
        ),
        (
            "no started label",
            found("a", "here:200", "", ""),
            false,
            "started label",
        ),
        (
            "stale, owner gone",
            found("a", "here:200", &old, "pid:[1]"),
            true,
            "gone",
        ),
        (
            "stale, owner gone in another PID namespace",
            found("a", "here:200", &old, "pid:[2]"),
            false,
            "PID namespace",
        ),
        (
            "stale, owner gone, no PID namespace label",
            found("a", "here:200", &old, ""),
            false,
            "PID namespace",
        ),
    ];
    let mut failures = Vec::new();
    for (name, env, remove, reason) in cases {
        let verdict = rules.decide(env);
        if verdict.remove != remove || !verdict.reason.contains(reason) {
            failures.push(format!(
                "{name}: remove={} ({}), want remove={remove} with {reason:?}",
                verdict.remove, verdict.reason
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Go: `TestStaleAfterFromEnv`. The variable comes through a lookup function rather than the
/// process environment, so the test runs in parallel with the others.
#[test]
fn stale_after_from_env_rejects_what_is_not_a_positive_duration() {
    let with =
        |value: &'static str| move |key: &str| (key == ENV_STALE_AFTER).then(|| value.to_owned());
    let read = |getenv| stale_after_from_env(ENV_STALE_AFTER, DEFAULT_STALE_AFTER, getenv);

    assert_eq!(DEFAULT_STALE_AFTER, Duration::from_hours(1));
    assert_eq!(
        stale_after_from_env(ENV_STALE_AFTER, DEFAULT_STALE_AFTER, |_| None),
        Ok(DEFAULT_STALE_AFTER),
        "unset"
    );
    assert_eq!(read(with("")), Ok(DEFAULT_STALE_AFTER), "empty");
    assert_eq!(read(with("90m")), Ok(Duration::from_mins(90)), "90m");
    // Another suite's variable is not this one's.
    assert_eq!(
        stale_after_from_env("SDA_IT_STALE_AFTER", DEFAULT_STALE_AFTER, with("90m")),
        Ok(DEFAULT_STALE_AFTER)
    );

    for bad in ["0s", "-1h", "soon", "10"] {
        let error = read(with(bad))
            .err()
            .unwrap_or_else(|| panic!("{bad:?} was accepted; a typo must not shorten the window"));
        assert_eq!(
            error.to_string(),
            format!(
                "bad stale-after window: {ENV_STALE_AFTER}=\"{bad}\" is not a positive duration"
            )
        );
    }
}

/// Go's `Duration.Round`, which the age in a keep reason goes through.
#[test]
fn round_matches_go() {
    const MINUTE: i64 = 60_000_000_000;
    assert_eq!(round(89_000_000_000, MINUTE), MINUTE);
    assert_eq!(
        round(90_000_000_000, MINUTE),
        2 * MINUTE,
        "a tie rounds away from zero"
    );
    assert_eq!(round(-90_000_000_000, MINUTE), -2 * MINUTE);
    assert_eq!(round(-89_000_000_000, MINUTE), -MINUTE);
    assert_eq!(round(i64::MAX, MINUTE), 9_223_372_020_000_000_000);
    assert_eq!(round(i64::MAX, 10), i64::MAX, "saturates");
    assert_eq!(round(i64::MIN, 10), i64::MIN, "saturates");
    assert_eq!(round(7, 0), 7);
}

/// The labels a run writes, and the environment a later sweep reads back from them.
#[test]
fn run_labels_are_read_back_by_record() {
    let owner = Owner {
        host: "ci".to_owned(),
        pid: 42,
        pid_namespace: "pid:[4026531836]".to_owned(),
    };
    let started = timestamp("2026-10-11T12:00:00.5Z");
    let labels = SSE_LABELS.run_labels("1a2b3c4d", &owner, started);
    assert_eq!(
        labels,
        HashMap::from([
            (SSE_LABELS.env_id.to_owned(), "1a2b3c4d".to_owned()),
            (SSE_LABELS.owner.to_owned(), "ci:42".to_owned()),
            // Whole seconds, UTC, as Go's time.RFC3339 of a UTC time.
            (
                SSE_LABELS.started.to_owned(),
                "2026-10-11T12:00:00Z".to_owned()
            ),
            (
                SSE_LABELS.pid_namespace.to_owned(),
                "pid:[4026531836]".to_owned()
            ),
        ])
    );

    let mut by_id = BTreeMap::new();
    // A helper with the envid alone, then the labelled resource: the owner comes from the second.
    let helper = HashMap::from([(SSE_LABELS.env_id.to_owned(), "1a2b3c4d".to_owned())]);
    SSE_LABELS.record(&mut by_id, Some(&helper));
    SSE_LABELS.record(&mut by_id, Some(&labels));
    // No labels, an empty envid and another suite's label name no environment.
    SSE_LABELS.record(&mut by_id, None);
    let empty = HashMap::from([(SSE_LABELS.env_id.to_owned(), String::new())]);
    SSE_LABELS.record(&mut by_id, Some(&empty));
    let other = HashMap::from([("swarm-device-access-it.envid".to_owned(), "x".to_owned())]);
    SSE_LABELS.record(&mut by_id, Some(&other));
    assert_eq!(
        by_id.into_values().collect::<Vec<_>>(),
        [Found {
            id: "1a2b3c4d".to_owned(),
            owner: "ci:42".to_owned(),
            started: "2026-10-11T12:00:00Z".to_owned(),
            pid_namespace: "pid:[4026531836]".to_owned(),
        }]
    );
}

/// Each suite's manual cleanup, byte for byte as it printed it.
#[test]
fn cleanup_commands_are_each_suite_s() {
    let filter = "label=swarm-scheduler-exporter.it.envid=1a2b3c4d";
    assert_eq!(
        Resources {
            remove_volumes: true,
            networks: true,
            images: false,
        }
        .cleanup_command(filter),
        format!(
            "docker ps -aq --filter {filter} | xargs -r docker rm -fv; docker network ls -q \
             --filter {filter} | xargs -r docker network rm"
        )
    );
    let filter = "label=swarm-device-access-it.envid=0123456789ab";
    assert_eq!(
        Resources {
            remove_volumes: false,
            networks: false,
            images: true,
        }
        .cleanup_command(filter),
        format!(
            "docker ps -aq --filter {filter} | xargs -r docker rm -f; docker image ls -q --filter \
             {filter} | xargs -r docker rmi -f"
        )
    );
}
