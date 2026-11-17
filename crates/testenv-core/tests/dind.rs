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

//! The `DinD` Swarm on a real host daemon, behind `TESTENV_CORE_DIND=1` (privileged containers;
//! `TESTENV_CORE_DIND_IMAGE` overrides the image, `docker:29.8.1-dind`). Without the variable
//! every test returns at once.
//!
//! - one manager and one worker reach Ready, and `down` leaves nothing;
//! - a setup step that fails with keep off leaves nothing;
//! - with keep on it leaves the run, every resource labelled with the keep label;
//! - an `up` dropped during the step still tears the run down.

#![expect(
    clippy::unwrap_used,
    reason = "the helpers fail the test on an unexpected error, as the tests themselves do"
)]

use std::collections::HashMap;
use std::io::Write as _;
use std::time::Duration;

use bollard::Docker;
use bollard::query_parameters::{ListContainersOptions, ListNetworksOptions};
use jiff::Timestamp;
use testenv_core::Error;
use testenv_core::context::Context;
use testenv_core::dind::{self, Cluster, Host, KEEP_VALUE, Names, OnFailure, Spec};
use testenv_core::envid::EnvIds;
use testenv_core::host;
use testenv_core::redact::Logger;
use testenv_core::sweep_rules::{Labels, Resources, current_owner};

const NAMES: Names = Names {
    env_ids: EnvIds {
        bytes: 4,
        label: "io.github.leinardi.testenv-core.envid",
        prefix: "tcdind",
    },
    labels: Labels {
        env_id: "io.github.leinardi.testenv-core.envid",
        owner: "io.github.leinardi.testenv-core.owner",
        started: "io.github.leinardi.testenv-core.started",
        pid_namespace: "io.github.leinardi.testenv-core.pidns",
    },
    keep_label: "io.github.leinardi.testenv-core.keep",
    resources: Resources {
        remove_volumes: true,
        networks: true,
        images: false,
    },
};

/// A line on stderr, where the test harness shows it for a failed test or under `--nocapture`.
fn say(line: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// Whether the smoke test runs; it says why not when it does not.
fn enabled() -> bool {
    let on = std::env::var("TESTENV_CORE_DIND").is_ok_and(|value| value == "1");
    if !on {
        say("skipped: set TESTENV_CORE_DIND=1 to bring up a DinD Swarm");
    }
    on
}

fn host_client() -> Docker {
    Docker::connect_with_defaults().unwrap()
}

fn spec(workers: usize, keep: OnFailure) -> Spec {
    Spec {
        env_id: NAMES.env_ids.generate().unwrap(),
        dind_image: std::env::var("TESTENV_CORE_DIND_IMAGE")
            .unwrap_or_else(|_| "docker:29.8.1-dind".to_owned()),
        managers: 1,
        workers,
        keep,
        names: NAMES,
        logger: Logger::new(say),
    }
}

fn host(env_id: &str) -> Host {
    let owner = current_owner().unwrap();
    Host {
        client: host_client(),
        rules: host::sweep_rules(Timestamp::now(), Duration::from_hours(1), env_id, &owner),
        owner,
    }
}

/// The containers and networks of the run `env_id` on the host, each with its labels.
async fn left(env_id: &str) -> Vec<HashMap<String, String>> {
    let client = host_client();
    let filters = HashMap::from([("label".to_owned(), vec![NAMES.env_ids.label_filter(env_id)])]);
    let containers = client
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters.clone()),
            ..ListContainersOptions::default()
        }))
        .await
        .unwrap();
    let networks = client
        .list_networks(Some(ListNetworksOptions {
            filters: Some(filters),
        }))
        .await
        .unwrap();
    containers
        .into_iter()
        .map(|container| container.labels.unwrap_or_default())
        .chain(
            networks
                .into_iter()
                .map(|network| network.labels.unwrap_or_default()),
        )
        .collect()
}

async fn failing_step(_: &Context, _: &Cluster) -> Result<(), Error> {
    Err(Error::new("the suite's setup failed"))
}

#[tokio::test]
async fn one_manager_and_one_worker_reach_ready_and_down_leaves_nothing() {
    if !enabled() {
        return;
    }
    let ctx = Context::background();
    let spec = spec(1, OnFailure::Remove);
    let env_id = spec.env_id.clone();
    let (cluster, ()) = dind::up(&ctx, spec, host(&env_id), async |_, _| Ok(()))
        .await
        .unwrap();
    assert_eq!(cluster.nodes.len(), 2);
    assert_eq!(cluster.workers().len(), 1);
    for node in &cluster.nodes {
        assert!(!node.swarm_node_id.is_empty(), "{node:?}");
        node.client().unwrap().ping().await.unwrap();
    }
    assert!(!left(&env_id).await.is_empty());
    cluster.down(&ctx).await.unwrap();
    assert!(left(&env_id).await.is_empty());
}

#[tokio::test]
async fn a_failed_setup_step_with_keep_off_leaves_nothing() {
    if !enabled() {
        return;
    }
    let ctx = Context::background();
    let spec = spec(0, OnFailure::Remove);
    let env_id = spec.env_id.clone();
    let error = dind::up(&ctx, spec, host(&env_id), failing_step)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("the suite's setup failed"), "{error}");
    assert!(error.contains(&env_id), "{error}");
    assert!(left(&env_id).await.is_empty());
}

#[tokio::test]
async fn a_failed_setup_step_with_keep_on_leaves_the_labelled_run() {
    if !enabled() {
        return;
    }
    let ctx = Context::background();
    let spec = spec(0, OnFailure::Keep);
    let env_id = spec.env_id.clone();
    dind::up(&ctx, spec, host(&env_id), failing_step)
        .await
        .unwrap_err();
    let kept = left(&env_id).await;
    // The manager and the network.
    assert_eq!(kept.len(), 2, "{kept:?}");
    for labels in &kept {
        assert_eq!(
            labels.get(NAMES.keep_label).map(String::as_str),
            Some(KEEP_VALUE),
            "{labels:?}"
        );
    }
    NAMES
        .host_sweep()
        .cleanup(&ctx, &host_client(), &env_id)
        .await
        .unwrap();
    assert!(left(&env_id).await.is_empty());
}

#[tokio::test]
async fn an_up_dropped_during_the_setup_step_still_tears_down() {
    if !enabled() {
        return;
    }
    let ctx = Context::background();
    let spec = spec(0, OnFailure::Remove);
    let env_id = spec.env_id.clone();
    let (reached, mut in_step) = tokio::sync::oneshot::channel::<()>();
    let reached = std::cell::Cell::new(Some(reached));
    let up = dind::up(&ctx, spec, host(&env_id), async |_, _| {
        if let Some(reached) = reached.take() {
            let _ = reached.send(());
        }
        std::future::pending::<Result<(), Error>>().await
    });
    tokio::select! {
        result = up => panic!("up returned from a step that never ends: {:?}", result.map(|_| ())),
        _ = &mut in_step => {}
    }
    // `up` is dropped here; its guard's removal runs as a task on this runtime.
    let gone = dind::poll_until(
        &ctx,
        Duration::from_mins(2),
        "the run removed",
        async || {
            let remaining = left(&env_id).await.len();
            if remaining == 0 {
                Ok(())
            } else {
                Err(format!("{remaining} resources left"))
            }
        },
    )
    .await;
    gone.unwrap();
}
