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

//! The spec's checks, the labels and the keep wiring, and the poll loop, without Docker. The
//! bring-up itself is the env-gated smoke test in `tests/dind.rs`.

use std::cell::Cell;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;

use super::{KEEP_VALUE, Names, OnFailure, Spec, Teardown, poll_until};
use crate::context::Context;
use crate::envid::EnvIds;
use crate::redact::Logger;
use crate::sweep_rules::{Labels, Owner, Resources};

const NAMES: Names = Names {
    env_ids: EnvIds {
        bytes: 4,
        label: "test.envid",
        prefix: "tst",
    },
    labels: Labels {
        env_id: "test.envid",
        owner: "test.owner",
        started: "test.started",
        pid_namespace: "test.pidns",
    },
    keep_label: "test.keep",
    resources: Resources {
        remove_volumes: true,
        networks: true,
        images: false,
    },
};

fn spec(keep: OnFailure) -> Spec {
    Spec {
        env_id: "1a2b3c4d".to_owned(),
        dind_image: "docker:29.8.1-dind".to_owned(),
        managers: 1,
        workers: 2,
        keep,
        names: NAMES,
        logger: Logger::new(|_| {}),
    }
}

fn owner() -> Owner {
    Owner {
        host: "host".to_owned(),
        pid: 42,
        pid_namespace: "pid:[4026531836]".to_owned(),
    }
}

#[test]
fn a_spec_with_one_manager_an_image_and_an_envid_passes() {
    assert!(spec(OnFailure::Remove).check().is_ok());
}

#[test]
fn a_spec_that_cannot_be_brought_up_is_refused() {
    for managers in [0, 2, 3] {
        let mut wrong = spec(OnFailure::Remove);
        wrong.managers = managers;
        let error = wrong.check().unwrap_err().to_string();
        assert!(error.contains(&format!("{managers} managers")), "{error}");
    }
    let mut no_image = spec(OnFailure::Remove);
    no_image.dind_image.clear();
    assert!(no_image.check().unwrap_err().to_string().contains("image"));
    let mut no_envid = spec(OnFailure::Remove);
    no_envid.env_id.clear();
    assert!(no_envid.check().unwrap_err().to_string().contains("envid"));
    let mut split = spec(OnFailure::Remove);
    split.names.labels.env_id = "other.envid";
    let error = split.check().unwrap_err().to_string();
    assert!(error.contains("other.envid"), "{error}");
}

/// Every resource carries the suite's envid, owner, start and namespace labels; the keep label
/// only when a failure keeps the run, with the value the keep-aware sweep reads.
#[test]
fn the_run_labels_are_the_suite_s_and_carry_the_keep_label_when_kept() {
    let started = "2026-01-02T03:04:05Z".parse::<Timestamp>().unwrap();
    let removed = spec(OnFailure::Remove).run_labels(&owner(), started);
    assert_eq!(
        removed.get("test.envid").map(String::as_str),
        Some("1a2b3c4d")
    );
    assert_eq!(
        removed.get("test.started").map(String::as_str),
        Some("2026-01-02T03:04:05Z")
    );
    assert_eq!(
        removed.get("test.pidns").map(String::as_str),
        Some("pid:[4026531836]")
    );
    assert!(removed.contains_key("test.owner"));
    assert!(!removed.contains_key("test.keep"));
    assert_eq!(removed.len(), 4);

    let kept = spec(OnFailure::Keep).run_labels(&owner(), started);
    assert_eq!(kept.get("test.keep").map(String::as_str), Some(KEEP_VALUE));
    assert_eq!(KEEP_VALUE, "true");
    assert_eq!(kept.len(), 5);
}

#[test]
fn the_cleanup_command_selects_the_run_by_the_suite_s_label() {
    assert_eq!(
        NAMES.cleanup_command("1a2b3c4d"),
        "docker ps -aq --filter label=test.envid=1a2b3c4d | xargs -r docker rm -fv; docker network \
         ls -q --filter label=test.envid=1a2b3c4d | xargs -r docker network rm"
    );
    let sweep = NAMES.host_sweep();
    assert_eq!(sweep.labels, NAMES.labels);
    assert_eq!(sweep.resources, NAMES.resources);
}

/// A logger that keeps its lines.
fn recording() -> (Logger, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let logger = Logger::new(move |line| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line.to_owned());
    });
    (logger, lines)
}

fn teardown(keep: OnFailure, log: Logger) -> Teardown {
    Teardown {
        armed: true,
        ctx: Context::background(),
        // Nothing listens there: a spawned removal fails at once, and logs it.
        client: bollard::Docker::connect_with_http(
            "tcp://127.0.0.1:1",
            1,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap(),
        names: NAMES,
        env_id: "1a2b3c4d".to_owned(),
        keep,
        log,
    }
}

/// A dropped bring-up that keeps its environment says so with the cleanup command and removes
/// nothing; one that is disarmed (`up` returned) does nothing at all.
#[test]
fn a_dropped_guard_keeps_a_kept_run_and_a_disarmed_one_does_nothing() {
    let (log, lines) = recording();
    drop(teardown(OnFailure::Keep, log));
    let logged = lines.lock().unwrap().join("\n");
    assert!(
        logged.contains("bring-up abandoned; keeping the environment"),
        "{logged}"
    );
    assert!(logged.contains("label=test.envid=1a2b3c4d"), "{logged}");

    let (log, lines) = recording();
    let mut disarmed = teardown(OnFailure::Remove, log);
    disarmed.armed = false;
    drop(disarmed);
    assert!(lines.lock().unwrap().is_empty());
}

/// Outside a runtime there is nothing to run the removal on: the guard says to remove it by hand.
#[test]
fn a_guard_dropped_outside_a_runtime_logs_the_cleanup_command() {
    let (log, lines) = recording();
    drop(teardown(OnFailure::Remove, log));
    let logged = lines.lock().unwrap().join("\n");
    assert!(logged.contains("outside a runtime"), "{logged}");
    assert!(logged.contains("docker ps -aq"), "{logged}");
}

/// Inside a runtime, a dropped guard hands the removal to a task.
#[tokio::test]
async fn a_guard_dropped_inside_a_runtime_spawns_the_removal() {
    let (log, lines) = recording();
    drop(teardown(OnFailure::Remove, log));
    let logged = lines.lock().unwrap().join("\n");
    assert!(
        logged.contains("bring-up abandoned; removing the environment"),
        "{logged}"
    );
}

#[tokio::test(start_paused = true)]
async fn poll_until_retries_until_the_check_passes() {
    let calls = Cell::new(0);
    poll_until(
        &Context::background(),
        Duration::from_secs(5),
        "ready",
        async || {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(format!("attempt {}", calls.get()))
            } else {
                Ok(())
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.get(), 3);
}

#[tokio::test(start_paused = true)]
async fn poll_until_names_what_it_waited_for_and_the_last_error() {
    let error = poll_until(
        &Context::background(),
        Duration::from_secs(2),
        "ready",
        async || Err("not yet".to_owned()),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "ready: not met within 2s: not yet");

    let ctx = Context::background().with_cancel();
    ctx.cancel();
    let error = poll_until(&ctx, Duration::from_secs(2), "ready", async || {
        Err("not yet".to_owned())
    })
    .await
    .unwrap_err();
    assert!(error.starts_with("ready: "), "{error}");
    assert!(error.ends_with(": not yet"), "{error}");
}
