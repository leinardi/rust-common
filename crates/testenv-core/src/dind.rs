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

//! A Docker Swarm of `DinD` nodes on the host daemon, for a suite's integration tests: a bridge
//! network, one manager and the asked number of workers as privileged `docker:dind` containers
//! serving plain TCP on 2375, Swarm init and join, the two readiness phases (every daemon answers
//! a ping, the manager lists every node Ready), and a keep-aware sweep before anything is created.
//!
//! What is a suite's own stays in the suite and comes in as configuration ([`Spec`], [`Host`]):
//! reading its environment (its variable names, how it parses a keep switch, its default image),
//! its label keys and keep label ([`Names`]), the sweep rules, its own setup (a workload image),
//! and the binary under test. A setup step that must run inside the bring-up's cleanup boundary is
//! [`up`]'s `after_swarm`: it gets the [`Cluster`] once every node is Ready, and a failure there is
//! a failed bring-up. That step is a parameter of [`up`] rather than a field of [`Spec`] so it can
//! hand back what it made (an image ID, say) without a box.
//!
//! A failed bring-up, the step included, removes everything it created unless the spec keeps it
//! ([`OnFailure::Keep`]): then every resource carries the keep label (`<keep_label>=true`), so no
//! later sweep removes it, and the cleanup command is logged. An [`up`] dropped before it returns
//! (its context's deadline, a cancelled task) tears down the same way, from a guard: the teardown is
//! handed to a task on the current tokio runtime, so it completes as long as that runtime keeps
//! running (a later run's sweep removes whatever a runtime cut short, once its owner is gone).
//!
//! Extracted from swarm-scheduler-exporter/testenv/src/dind.rs and cluster.rs at c638318 (the
//! cluster's types, `pause_node`, `unpause_node`, `ensure_host_image` on the cluster, the node's
//! host port) and runhold/testenv/src/dind.rs, cluster.rs and wait.rs at 5929b058 (the network
//! created before the image pull, the keep label on every resource, the keep-aware sweep, the
//! `poll_until` texts). The SSE copy is MIT; Runhold's adaptation of it is GPL-3.0-or-later. Both
//! are by the same sole author, who relicenses the extracted lines here under MIT OR Apache-2.0.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::models::{
    ContainerCreateBody, EndpointSettings, HostConfig, NetworkCreateRequest, NetworkingConfig,
    NodeState, PortBinding, SwarmInitRequest, SwarmJoinRequest,
};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, InspectContainerOptions, ListNodesOptions,
    StartContainerOptions,
};
use futures_util::StreamExt as _;
use jiff::Timestamp;
use tokio::time::{Instant, MissedTickBehavior};

use crate::Error;
use crate::context::Context;
use crate::envid::EnvIds;
use crate::host::{HostSweep, OP_TIMEOUT, with_timeout};
use crate::redact::{Log as _, Logger};
use crate::sweep_rules::{Labels, Owner, Resources, SweepRules};

const DIND_DAEMON_PORT: &str = "2375/tcp";
const SWARM_MANAGE_PORT: &str = "2377";
const DAEMON_READY_WAIT: Duration = Duration::from_secs(60);
const SWARM_READY_WAIT: Duration = Duration::from_secs(90);

/// How often [`poll_until`] re-checks its condition, as Go's `dindPollInterval`.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Bounds swarm init and join, which wait for Raft and TLS bootstrap.
const SWARM_OP_TIMEOUT: Duration = Duration::from_secs(90);
/// Bounds one image pull, and is the nodes' clients' own timeout.
pub const IMAGE_TRANSFER_TIMEOUT: Duration = Duration::from_mins(5);
/// Bounds the removal of a failed bring-up.
const TEARDOWN_TIMEOUT: Duration = Duration::from_mins(2);
/// The value of the keep label on a kept run's resources, the one the keep-aware sweep reads.
pub const KEEP_VALUE: &str = "true";

/// How one suite names and finds its runs on the host: the envid and resource names, the labels
/// a run records itself with, the label that keeps a failed run, and what a cleanup removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Names {
    /// The envid's size, label and resource-name prefix.
    pub env_ids: EnvIds,
    /// The owner, start time and PID-namespace labels.
    pub labels: Labels,
    /// The label (`<keep_label>=true`) that keeps a failed run's resources from every sweep.
    pub keep_label: &'static str,
    /// What a cleanup removes beside the containers; a cluster leaves a network.
    pub resources: Resources,
}

impl Names {
    /// The sweep and cleanup of these runs on the host daemon, each call bounded by
    /// [`OP_TIMEOUT`].
    #[must_use]
    pub const fn host_sweep(&self) -> HostSweep {
        HostSweep {
            labels: self.labels,
            resources: self.resources,
            op_timeout: OP_TIMEOUT,
        }
    }

    /// The shell command that removes every resource of the run `env_id` by hand.
    #[must_use]
    pub fn cleanup_command(&self, env_id: &str) -> String {
        self.resources
            .cleanup_command(&self.env_ids.cli_filter(env_id))
    }
}

/// Whether a failed run leaves its environment in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnFailure {
    /// Remove everything, as a passing run does.
    Remove,
    /// Keep it for debugging, every resource labelled with [`Names::keep_label`].
    Keep,
}

/// The Swarm role a node was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// The Swarm manager.
    Manager,
    /// A Swarm worker.
    Worker,
}

/// The requested topology of a test cluster, and how the suite names it.
#[derive(Debug, Clone)]
pub struct Spec {
    /// Namespaces every resource of the run ([`EnvIds::generate`]).
    pub env_id: String,
    /// The `docker:dind` image every node runs; the suite picks its default.
    pub dind_image: String,
    /// The number of managers: only 1 is brought up, any other count is refused.
    pub managers: usize,
    /// The number of workers.
    pub workers: usize,
    /// What a failed bring-up or a failed run leaves.
    pub keep: OnFailure,
    /// The suite's names and labels.
    pub names: Names,
    /// Receives the harness's progress.
    pub logger: Logger,
}

impl Spec {
    /// Whether a failure keeps the environment.
    #[must_use]
    pub fn keeps_on_failure(&self) -> bool {
        self.keep == OnFailure::Keep
    }

    /// Refuses a spec that cannot be brought up: no envid, no image, a manager count other than
    /// one, or names whose envid labels disagree (the sweep finds runs by [`Labels::env_id`], the
    /// cleanup command by [`EnvIds::label`]).
    ///
    /// # Errors
    ///
    /// The first field that is wrong, named.
    pub fn check(&self) -> Result<(), Error> {
        if self.env_id.is_empty() {
            return Err(Error::new("spec: the envid is empty"));
        }
        if self.dind_image.is_empty() {
            return Err(Error::new("spec: the dind image is empty"));
        }
        if self.names.env_ids.label != self.names.labels.env_id {
            return Err(Error::new(format!(
                "spec: the envid label is {:?} for the names but {:?} for the labels",
                self.names.env_ids.label, self.names.labels.env_id
            )));
        }
        if self.managers != 1 {
            return Err(Error::new(format!(
                "spec: {} managers asked for; the harness brings up exactly one",
                self.managers
            )));
        }
        Ok(())
    }

    /// The labels every resource of the run carries: the envid, the owner, the start time and
    /// the PID namespace, and the keep label when a failure keeps the run (Go's `envLabels`).
    #[must_use]
    pub fn run_labels(&self, owner: &Owner, started_at: Timestamp) -> HashMap<String, String> {
        let mut labels = self
            .names
            .labels
            .run_labels(&self.env_id, owner, started_at);
        if self.keeps_on_failure() {
            labels.insert(self.names.keep_label.to_owned(), KEEP_VALUE.to_owned());
        }
        labels
    }
}

/// The host daemon and this run's place on it: the client, the sweep rules (whose `now` is the
/// run's start, recorded in its labels) and the process that owns the run.
pub struct Host {
    /// The host daemon's client, the suite's.
    pub client: Docker,
    /// The rules of the sweep [`up`] runs before anything is created.
    pub rules: SweepRules,
    /// This process, recorded in the owner label.
    pub owner: Owner,
}

/// One member of the cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The `DinD` container name on the host.
    pub name: String,
    /// The `DinD` container ID on the host, the target of [`Cluster::pause_node`].
    pub container_id: String,
    /// The container hostname, which Swarm reports as the node hostname.
    pub hostname: String,
    /// The Swarm role the node was given at bring-up.
    pub role: NodeRole,
    /// The node's ID inside the test Swarm.
    pub swarm_node_id: String,
    /// The 127.0.0.1 port on the host mapped to the node's daemon.
    pub host_port: u16,
    /// The `DOCKER_HOST` value that reaches the node's daemon from the host.
    pub docker_host: String,
    /// The node's address on the run's bridge network, which Swarm advertises.
    pub bridge_ip: String,
}

impl Node {
    /// A client for the node's daemon, over plain HTTP. It is built from the node's address
    /// alone, never from the environment, so a `DOCKER_*` variable in the caller's shell (TLS, a
    /// context) cannot redirect or break it. Its requests are unversioned (bollard drops the
    /// prefix), so the daemon serves each at its own API version.
    ///
    /// # Errors
    ///
    /// The address is not one bollard can dial.
    pub fn client(&self) -> Result<Docker, Error> {
        Docker::connect_with_http(
            &self.docker_host,
            IMAGE_TRANSFER_TIMEOUT.as_secs(),
            bollard::API_DEFAULT_VERSION,
        )
        .map_err(|error| {
            Error::new(format!(
                "create Docker client for node {:?}: {error}",
                self.name
            ))
        })
    }
}

/// A running test Swarm returned by [`up`]. Callers own it and call [`Cluster::down`].
#[derive(Debug)]
pub struct Cluster {
    /// What was asked for.
    pub spec: Spec,
    /// The manager, also first in [`Cluster::nodes`].
    pub manager: Node,
    /// The manager first, then the workers in order.
    pub nodes: Vec<Node>,
    /// When bring-up began, the start time its labels carry.
    pub started_at: Timestamp,
    host_client: Docker,
    owner: Owner,
}

impl Cluster {
    /// The labels every resource of the cluster carries ([`Spec::run_labels`]). Label what a test
    /// creates beside the cluster with them, so `down` removes it even when the test's own
    /// cleanup never runs.
    #[must_use]
    pub fn labels(&self) -> HashMap<String, String> {
        self.spec.run_labels(&self.owner, self.started_at)
    }

    /// The client of the host daemon the nodes run on.
    #[must_use]
    pub fn host_client(&self) -> &Docker {
        &self.host_client
    }

    /// The process that brought the cluster up.
    #[must_use]
    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    /// The worker nodes, in bring-up order.
    #[must_use]
    pub fn workers(&self) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|node| node.role == NodeRole::Worker)
            .collect()
    }

    /// Removes every container and network of the cluster. Pass a context that is not the one
    /// that may have expired during the tests: teardown must still run.
    ///
    /// # Errors
    ///
    /// Every removal that failed, joined.
    pub async fn down(&self, ctx: &Context) -> Result<(), Error> {
        self.spec
            .names
            .host_sweep()
            .cleanup(ctx, &self.host_client, &self.spec.env_id)
            .await
    }

    /// Pulls `image_ref` on the host daemon unless it is already there.
    ///
    /// # Errors
    ///
    /// The inspect or the pull failed.
    pub async fn ensure_host_image(&self, ctx: &Context, image_ref: &str) -> Result<(), Error> {
        ensure_host_image(ctx, &self.host_client, image_ref, &self.spec.logger).await
    }

    /// Freezes the node's `DinD` container. Its daemon stops answering, so the manager marks the
    /// node down once heartbeats lapse, while the tasks it ran keep their last reported state.
    ///
    /// # Errors
    ///
    /// The pause failed.
    pub async fn pause_node(&self, ctx: &Context, node: &Node) -> Result<(), Error> {
        with_timeout(
            ctx,
            OP_TIMEOUT,
            self.host_client.pause_container(&node.container_id),
        )
        .await
        .map_err(|error| Error::new(format!("pause node {:?}: {error}", node.name)))
    }

    /// Resumes a node frozen by [`Cluster::pause_node`]. Unpausing a node that is not paused is
    /// not an error, so cleanup can call it unconditionally.
    ///
    /// # Errors
    ///
    /// The inspect or the unpause failed.
    pub async fn unpause_node(&self, ctx: &Context, node: &Node) -> Result<(), Error> {
        let inspected = with_timeout(
            ctx,
            OP_TIMEOUT,
            self.host_client
                .inspect_container(&node.container_id, None::<InspectContainerOptions>),
        )
        .await
        .map_err(|error| Error::new(format!("inspect node {:?}: {error}", node.name)))?;
        let paused = inspected
            .state
            .and_then(|state| state.paused)
            .unwrap_or(false);
        if !paused {
            return Ok(());
        }
        with_timeout(
            ctx,
            OP_TIMEOUT,
            self.host_client.unpause_container(&node.container_id),
        )
        .await
        .map_err(|error| Error::new(format!("unpause node {:?}: {error}", node.name)))
    }
}

/// Brings up a fresh cluster for `spec` on `host`, then runs `after_swarm` on it: a bridge
/// network, one manager and `spec.workers` workers as privileged `DinD` containers, and a Swarm
/// whose nodes are all Ready. Before anything is created, the environments earlier runs abandoned
/// are swept ([`HostSweep::sweep_keeping`] with `host.rules` and the keep label), keeping those a
/// failed run kept.
///
/// If bring-up or `after_swarm` fails, everything created is removed before this returns, unless
/// the spec keeps it on failure: then it is left in place, labelled with the keep label, and the
/// cleanup command is logged. A dropped `up` does the same from a guard, as the module says.
///
/// # Errors
///
/// A spec [`Spec::check`] refuses, before anything is created; else the step that failed, with
/// the envid.
pub async fn up<T>(
    ctx: &Context,
    spec: Spec,
    host: Host,
    after_swarm: impl AsyncFnOnce(&Context, &Cluster) -> Result<T, Error>,
) -> Result<(Cluster, T), Error> {
    spec.check()?;
    let names = spec.names;
    let log = spec
        .logger
        .clone()
        .with_fields(&[("envid", spec.env_id.as_str())]);
    // Logged before anything exists, so a run killed at any later point can still be cleaned up
    // by hand.
    log.info(
        "bringing up test swarm; manual cleanup",
        &[("cmd", names.cleanup_command(&spec.env_id).as_str())],
    );
    let Host {
        client: host_client,
        rules,
        owner,
    } = host;
    let started_at = rules.now;
    names
        .host_sweep()
        .sweep_keeping(ctx, &host_client, &rules, names.keep_label, &log)
        .await;

    let mut guard = Teardown {
        armed: true,
        ctx: ctx.clone(),
        client: host_client.clone(),
        names,
        env_id: spec.env_id.clone(),
        keep: spec.keep,
        log: log.clone(),
    };
    let result = async {
        let nodes = bring_up(ctx, &spec, &owner, started_at, &host_client, &log).await?;
        let cluster = Cluster {
            manager: nodes[0].clone(),
            nodes,
            started_at,
            host_client: host_client.clone(),
            owner: owner.clone(),
            spec: spec.clone(),
        };
        let made = after_swarm(ctx, &cluster).await?;
        Ok::<_, Error>((cluster, made))
    }
    .await;
    match result {
        Ok(done) => {
            guard.armed = false;
            Ok(done)
        }
        Err(error) => {
            // Disarmed only once the removal is done: an `up` dropped during it hands the removal
            // to the guard again (removing what is already gone is not an error).
            guard.after_failure(&error).await;
            guard.armed = false;
            Err(error.context(format!("bring up test swarm {}", spec.env_id)))
        }
    }
}

/// What a failed or dropped bring-up leaves: the environment kept, or removed.
struct Teardown {
    armed: bool,
    ctx: Context,
    client: Docker,
    names: Names,
    env_id: String,
    keep: OnFailure,
    log: Logger,
}

impl Teardown {
    /// Keeps or removes the environment after `error`, before [`up`] returns it.
    async fn after_failure(&self, error: &Error) {
        if self.keep == OnFailure::Keep {
            self.log.warn(
                "bring-up failed; keeping the environment",
                &[
                    ("err", error.to_string().as_str()),
                    ("cmd", self.names.cleanup_command(&self.env_id).as_str()),
                ],
            );
            return;
        }
        let teardown = self.ctx.without_cancel().with_timeout(TEARDOWN_TIMEOUT);
        remove(&teardown, &self.client, self.names, &self.env_id, &self.log).await;
    }
}

impl Drop for Teardown {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.keep == OnFailure::Keep {
            self.log.warn(
                "bring-up abandoned; keeping the environment",
                &[("cmd", self.names.cleanup_command(&self.env_id).as_str())],
            );
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.log.warn(
                "bring-up abandoned outside a runtime; remove it by hand",
                &[("cmd", self.names.cleanup_command(&self.env_id).as_str())],
            );
            return;
        };
        self.log
            .warn("bring-up abandoned; removing the environment", &[]);
        let teardown = self.ctx.without_cancel().with_timeout(TEARDOWN_TIMEOUT);
        let client = self.client.clone();
        let names = self.names;
        let env_id = std::mem::take(&mut self.env_id);
        let log = self.log.clone();
        drop(runtime.spawn(async move {
            remove(&teardown, &client, names, &env_id, &log).await;
        }));
    }
}

/// Removes every resource of the run `env_id`, logging a cleanup that did not finish.
async fn remove(ctx: &Context, client: &Docker, names: Names, env_id: &str, log: &Logger) {
    if let Err(error) = names.host_sweep().cleanup(ctx, client, env_id).await {
        log.warn(
            "bring-up failed and cleanup was incomplete",
            &[("err", error.to_string().as_str())],
        );
    }
}

// --- Bring-up ---

async fn bring_up(
    ctx: &Context,
    spec: &Spec,
    owner: &Owner,
    started_at: Timestamp,
    host_client: &Docker,
    log: &Logger,
) -> Result<Vec<Node>, Error> {
    // The network comes first, the image after it (Go pulls first): a pull that fails then leaves
    // a labelled resource, which is how a failed bring-up's retention is checked by hand.
    let labels = spec.run_labels(owner, started_at);
    let network_name = spec.names.env_ids.resource_name(&spec.env_id, "net");
    create_bridge_network(ctx, host_client, &network_name, &labels).await?;

    ensure_host_image(ctx, host_client, &spec.dind_image, log).await?;

    let mut nodes = provision_nodes(ctx, host_client, spec, &network_name, &labels).await?;
    log.info(
        "DinD containers started",
        &[("count", nodes.len().to_string().as_str())],
    );

    wait_for_daemons(ctx, &nodes).await?;
    log.info("inner daemons ready", &[]);

    let worker_token = init_swarm(ctx, &nodes[0]).await?;
    join_workers(ctx, &nodes[0].bridge_ip, &nodes[1..], &worker_token).await?;
    wait_for_swarm_ready(ctx, &nodes[0], nodes.len()).await?;
    record_swarm_node_ids(ctx, &mut nodes).await?;

    log.info(
        "all swarm nodes ready",
        &[("nodes", nodes.len().to_string().as_str())],
    );
    Ok(nodes)
}

async fn create_bridge_network(
    ctx: &Context,
    host_client: &Docker,
    name: &str,
    labels: &HashMap<String, String>,
) -> Result<(), Error> {
    with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.create_network(NetworkCreateRequest {
            name: name.to_owned(),
            driver: Some("bridge".to_owned()),
            labels: Some(labels.clone()),
            ..NetworkCreateRequest::default()
        }),
    )
    .await
    .map(|_| ())
    .map_err(|error| Error::new(format!("create bridge network {name:?}: {error}")))
}

async fn provision_nodes(
    ctx: &Context,
    host_client: &Docker,
    spec: &Spec,
    network_name: &str,
    labels: &HashMap<String, String>,
) -> Result<Vec<Node>, Error> {
    let mut nodes = Vec::with_capacity(spec.workers + 1);
    nodes.push(
        start_node(
            ctx,
            host_client,
            spec,
            labels,
            network_name,
            "manager-0",
            NodeRole::Manager,
        )
        .await?,
    );
    for index in 0..spec.workers {
        nodes.push(
            start_node(
                ctx,
                host_client,
                spec,
                labels,
                network_name,
                &format!("worker-{index}"),
                NodeRole::Worker,
            )
            .await?,
        );
    }
    Ok(nodes)
}

async fn start_node(
    ctx: &Context,
    host_client: &Docker,
    spec: &Spec,
    labels: &HashMap<String, String>,
    network_name: &str,
    hostname: &str,
    role: NodeRole,
) -> Result<Node, Error> {
    let name = spec.names.env_ids.resource_name(&spec.env_id, hostname);
    let body = ContainerCreateBody {
        image: Some(spec.dind_image.clone()),
        hostname: Some(hostname.to_owned()),
        // An empty DOCKER_TLS_CERTDIR makes the entrypoint serve plain TCP on 2375. The explicit
        // --tls=false matters too: without it dockerd deliberately stalls its start-up for
        // seconds to warn about unauthenticated TCP.
        env: Some(vec!["DOCKER_TLS_CERTDIR=".to_owned()]),
        cmd: Some(vec!["--tls=false".to_owned()]),
        labels: Some(labels.clone()),
        exposed_ports: Some(vec![DIND_DAEMON_PORT.to_owned()]),
        host_config: Some(HostConfig {
            privileged: Some(true),
            port_bindings: Some(HashMap::from([(
                DIND_DAEMON_PORT.to_owned(),
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_owned()),
                    host_port: Some("0".to_owned()),
                }]),
            )])),
            ..HostConfig::default()
        }),
        networking_config: Some(NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                network_name.to_owned(),
                EndpointSettings::default(),
            )])),
        }),
        ..ContainerCreateBody::default()
    };
    let created = with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.create_container(
            Some(CreateContainerOptions {
                name: Some(name.clone()),
                ..CreateContainerOptions::default()
            }),
            body,
        ),
    )
    .await
    .map_err(|error| Error::new(format!("create node {name:?}: {error}")))?;

    with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.start_container(&created.id, None::<StartContainerOptions>),
    )
    .await
    .map_err(|error| Error::new(format!("start node {name:?}: {error}")))?;

    inspect_node(
        ctx,
        host_client,
        created.id,
        name,
        hostname,
        role,
        network_name,
    )
    .await
}

async fn inspect_node(
    ctx: &Context,
    host_client: &Docker,
    container_id: String,
    name: String,
    hostname: &str,
    role: NodeRole,
    network_name: &str,
) -> Result<Node, Error> {
    let inspected = with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.inspect_container(&container_id, None::<InspectContainerOptions>),
    )
    .await
    .map_err(|error| Error::new(format!("inspect node {name:?}: {error}")))?;

    let not_attached = || {
        Error::new(format!(
            "node {name:?} on network {network_name:?}: network attachment missing"
        ))
    };
    let settings = inspected.network_settings.ok_or_else(not_attached)?;

    let bridge_ip = settings
        .networks
        .as_ref()
        .and_then(|networks| networks.get(network_name))
        .and_then(|endpoint| endpoint.ip_address.as_deref())
        .filter(|address| address.parse::<IpAddr>().is_ok())
        .ok_or_else(not_attached)?
        .to_owned();

    let host_port_text = settings
        .ports
        .as_ref()
        .and_then(|ports| ports.get(DIND_DAEMON_PORT))
        .and_then(Option::as_ref)
        .and_then(|bindings| bindings.first())
        .map(|binding| binding.host_port.clone().unwrap_or_default())
        .ok_or_else(|| {
            Error::new(format!(
                "node {name:?} port {DIND_DAEMON_PORT}: host port binding missing"
            ))
        })?;
    let host_port: u16 = host_port_text.parse().map_err(|error| {
        Error::new(format!(
            "node {name:?}: parse host port {host_port_text:?}: {error}"
        ))
    })?;

    Ok(Node {
        name,
        container_id,
        hostname: hostname.to_owned(),
        role,
        swarm_node_id: String::new(),
        host_port,
        docker_host: format!("tcp://127.0.0.1:{host_port}"),
        bridge_ip,
    })
}

/// The first readiness phase: every inner daemon answers a ping.
async fn wait_for_daemons(ctx: &Context, nodes: &[Node]) -> Result<(), Error> {
    for node in nodes {
        let client = node.client()?;
        poll_until(
            ctx,
            DAEMON_READY_WAIT,
            &format!("daemon on {} answers ping", node.name),
            async || {
                with_timeout(ctx, OP_TIMEOUT, client.ping())
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .map_err(Error::new)?;
    }
    Ok(())
}

async fn init_swarm(ctx: &Context, manager: &Node) -> Result<String, Error> {
    let client = manager.client()?;
    with_timeout(
        ctx,
        SWARM_OP_TIMEOUT,
        client.init_swarm(SwarmInitRequest {
            listen_addr: Some(format!("0.0.0.0:{SWARM_MANAGE_PORT}")),
            advertise_addr: Some(manager.bridge_ip.clone()),
            ..SwarmInitRequest::default()
        }),
    )
    .await
    .map_err(|error| Error::new(format!("swarm init on {:?}: {error}", manager.name)))?;

    let inspected = with_timeout(ctx, OP_TIMEOUT, client.inspect_swarm())
        .await
        .map_err(|error| Error::new(format!("swarm inspect on {:?}: {error}", manager.name)))?;
    inspected
        .join_tokens
        .and_then(|tokens| tokens.worker)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            Error::new(format!(
                "swarm inspect on {:?}: no worker join token",
                manager.name
            ))
        })
}

async fn join_workers(
    ctx: &Context,
    manager_bridge_ip: &str,
    workers: &[Node],
    worker_token: &str,
) -> Result<(), Error> {
    let remote_addr = format!("{manager_bridge_ip}:{SWARM_MANAGE_PORT}");
    for worker in workers {
        let client = worker.client()?;
        with_timeout(
            ctx,
            SWARM_OP_TIMEOUT,
            client.join_swarm(SwarmJoinRequest {
                listen_addr: Some(format!("0.0.0.0:{SWARM_MANAGE_PORT}")),
                advertise_addr: Some(worker.bridge_ip.clone()),
                remote_addrs: Some(vec![remote_addr.clone()]),
                join_token: Some(worker_token.to_owned()),
                ..SwarmJoinRequest::default()
            }),
        )
        .await
        .map_err(|error| Error::new(format!("swarm join on {:?}: {error}", worker.name)))?;
    }
    Ok(())
}

/// The second readiness phase: the manager lists every node as Ready.
async fn wait_for_swarm_ready(
    ctx: &Context,
    manager: &Node,
    total_nodes: usize,
) -> Result<(), Error> {
    let client = manager.client()?;
    poll_until(
        ctx,
        SWARM_READY_WAIT,
        &format!("{total_nodes} swarm nodes Ready"),
        async || {
            let listed = with_timeout(ctx, OP_TIMEOUT, client.list_nodes(None::<ListNodesOptions>))
                .await
                .map_err(|error| format!("node list: {error}"))?;
            let ready = listed
                .iter()
                .filter(|node| {
                    node.status.as_ref().and_then(|status| status.state) == Some(NodeState::READY)
                })
                .count();
            if ready == total_nodes {
                return Ok(());
            }
            Err(format!("{ready} of {total_nodes} nodes Ready"))
        },
    )
    .await
    .map_err(Error::new)
}

async fn record_swarm_node_ids(ctx: &Context, nodes: &mut [Node]) -> Result<(), Error> {
    for node in nodes {
        let client = node.client()?;
        let info = with_timeout(ctx, OP_TIMEOUT, client.info())
            .await
            .map_err(|error| Error::new(format!("info on {:?}: {error}", node.name)))?;
        node.swarm_node_id = info
            .swarm
            .and_then(|swarm| swarm.node_id)
            .unwrap_or_default();
    }
    Ok(())
}

/// Makes `image_ref` available on the host daemon, pulling it only when it is missing.
///
/// # Errors
///
/// The inspect failed for another reason than a missing image, or the pull failed.
pub async fn ensure_host_image(
    ctx: &Context,
    host_client: &Docker,
    image_ref: &str,
    log: &Logger,
) -> Result<(), Error> {
    match with_timeout(ctx, OP_TIMEOUT, host_client.inspect_image(image_ref)).await {
        Ok(_) => return Ok(()),
        Err(error) if !error.is_not_found() => {
            return Err(Error::new(format!(
                "inspect host image {image_ref:?}: {error}"
            )));
        }
        Err(_) => {}
    }

    log.info("pulling image on host", &[("image", image_ref)]);

    // The whole reference goes in fromImage: the daemon splits off the tag or digest itself.
    let pull = async {
        let mut progress = host_client.create_image(
            Some(CreateImageOptions {
                from_image: Some(image_ref.to_owned()),
                ..CreateImageOptions::default()
            }),
            None,
            None,
        );
        while let Some(message) = progress.next().await {
            message?;
        }
        Ok::<(), DockerError>(())
    };
    with_timeout(ctx, IMAGE_TRANSFER_TIMEOUT, pull)
        .await
        .map_err(|error| Error::new(format!("pull host image {image_ref:?}: {error}")))
}

/// Calls `check` every [`POLL_INTERVAL`] until it succeeds, `timeout` elapses or `ctx` ends.
///
/// # Errors
///
/// `what`, then why the wait ended (`not met within <timeout>`, or the context's end), then the
/// check's last error.
pub async fn poll_until(
    ctx: &Context,
    timeout: Duration,
    what: &str,
    mut check: impl AsyncFnMut() -> Result<(), String>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // The first tick is immediate; Go's ticker first fires after one interval.
    ticker.tick().await;
    loop {
        let Err(last) = check().await else {
            return Ok(());
        };
        let tick = tokio::time::timeout_at(deadline, ticker.tick());
        match ctx.run(tick).await {
            Err(ended) => return Err(format!("{what}: {ended}: {last}")),
            Ok(Err(_)) => return Err(format!("{what}: not met within {timeout:?}: {last}")),
            Ok(Ok(_)) => {}
        }
    }
}

#[cfg(test)]
mod tests;
