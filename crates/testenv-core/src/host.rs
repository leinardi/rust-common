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

//! What a harness does on the host Docker daemon itself, rather than in a test, on bollard: the
//! sweep of abandoned runs, the cleanup of one run, the removals that tolerate a resource already
//! gone, and one call bounded under a context. The client, the run's own resources and anything a
//! suite brings up stay the suite's.
//!
//! A [`HostSweep`] holds what differs between the suites:
//!
//! - swarm-scheduler-exporter: its [`Labels`], [`Resources`] with `remove_volumes` and `networks`,
//!   [`OP_TIMEOUT`];
//! - swarm-device-access: its [`Labels`], [`Resources`] with `images`, [`OP_TIMEOUT`].
//!
//! Extracted from swarm-scheduler-exporter/testenv/src/dind.rs at c9e79afa8810 (`sweep`, `record`,
//! `list_environments` and `cleanup` over containers and networks, `process_alive`, `OpError`,
//! `with_timeout`) and swarm-device-access/testenv/src/host.rs at 8b4ab3a3e417 (the same over
//! containers and images, `remove_container`, `remove_image`, `default_sweep_rules`).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::future::Future;
use std::time::Duration;

use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::query_parameters::{
    ListContainersOptions, ListImagesOptions, ListNetworksOptions, RemoveContainerOptions,
    RemoveImageOptions,
};
use go_slog::Attr;
use jiff::Timestamp;

use crate::Error;
use crate::context::{Context, Ended};
use crate::redact::RedactingLogger;
use crate::sweep_rules::{Found, Labels, Owner, Resources, SweepRules};

/// Bounds one ordinary Docker call in both suites, so a hung daemon fails that call instead of
/// stalling the run until the suite deadline.
pub const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// One suite's sweep and cleanup on the host daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostSweep {
    /// The labels a run's resources carry.
    pub labels: Labels,
    /// What a run leaves beside its containers.
    pub resources: Resources,
    /// The bound of each call ([`OP_TIMEOUT`]).
    pub op_timeout: Duration,
}

impl HostSweep {
    /// Removes the runs that `rules` finds abandoned: leaks from runs that were killed before
    /// their cleanup ran. A run that may still belong to a running suite, on this host or another,
    /// is left alone, and the reason is logged at debug; a removal is logged at info, and a
    /// cleanup that did not finish at warn.
    pub async fn sweep(
        &self,
        ctx: &Context,
        client: &Docker,
        rules: &SweepRules,
        log: &RedactingLogger,
    ) {
        for env in self.list_environments(ctx, client, log).await {
            let verdict = rules.decide(env);
            if !verdict.remove {
                if verdict.env.id != rules.self_id {
                    log.debug(
                        "sweep: keeping environment",
                        &[
                            Attr::string("other_envid", verdict.env.id.clone()),
                            Attr::string("reason", verdict.reason.clone()),
                        ],
                    );
                }
                continue;
            }

            log.info(
                "sweep: removing leaked environment",
                &[
                    Attr::string("leaked_envid", verdict.env.id.clone()),
                    Attr::string("reason", verdict.reason.clone()),
                ],
            );

            if let Err(error) = self.cleanup(ctx, client, &verdict.env.id).await {
                log.warn(
                    "sweep: cleanup incomplete",
                    &[
                        Attr::string("leaked_envid", verdict.env.id.clone()),
                        Attr::string("err", error.to_string()),
                    ],
                );
            }
        }
    }

    /// Every environment with a container (or a network or image, as configured) on the host
    /// daemon, from their labels. A listing that fails is logged at warn and leaves that kind of
    /// resource out: the sweep is best-effort, and each run still removes its own resources.
    pub async fn list_environments(
        &self,
        ctx: &Context,
        client: &Docker,
        log: &RedactingLogger,
    ) -> Vec<Found> {
        let filters = label_filters(self.labels.env_id.to_owned());
        let mut by_id = BTreeMap::new();

        match with_timeout(
            ctx,
            self.op_timeout,
            client.list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters.clone()),
                ..ListContainersOptions::default()
            })),
        )
        .await
        {
            Ok(containers) => {
                for container in &containers {
                    self.labels.record(&mut by_id, container.labels.as_ref());
                }
            }
            Err(error) => log.warn(
                "sweep: list containers",
                &[Attr::string("err", error.to_string())],
            ),
        }

        if self.resources.networks {
            match with_timeout(
                ctx,
                self.op_timeout,
                client.list_networks(Some(ListNetworksOptions {
                    filters: Some(filters.clone()),
                })),
            )
            .await
            {
                Ok(networks) => {
                    for network in &networks {
                        self.labels.record(&mut by_id, network.labels.as_ref());
                    }
                }
                Err(error) => log.warn(
                    "sweep: list networks",
                    &[Attr::string("err", error.to_string())],
                ),
            }
        }

        if self.resources.images {
            match with_timeout(
                ctx,
                self.op_timeout,
                client.list_images(Some(ListImagesOptions {
                    filters: Some(filters),
                    ..ListImagesOptions::default()
                })),
            )
            .await
            {
                Ok(images) => {
                    for image in &images {
                        self.labels.record(&mut by_id, Some(&image.labels));
                    }
                }
                Err(error) => log.warn(
                    "sweep: list images",
                    &[Attr::string("err", error.to_string())],
                ),
            }
        }

        by_id.into_values().collect()
    }

    /// Removes every container (with its anonymous volumes, if configured), then every network
    /// and image (as configured), labelled with `env_id`. It keeps going past individual
    /// failures and returns them joined.
    ///
    /// # Errors
    ///
    /// Every listing or removal that failed, one per line (`list containers: …`,
    /// `remove container <id>: …`, `list networks: …`, `remove network <id>: …`,
    /// `list images: …`, `remove image <id>: …`); a resource already gone is not a failure.
    pub async fn cleanup(&self, ctx: &Context, client: &Docker, env_id: &str) -> Result<(), Error> {
        let filters = label_filters(format!("{}={env_id}", self.labels.env_id));
        let mut errors = Vec::new();

        let containers = with_timeout(
            ctx,
            self.op_timeout,
            client.list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters.clone()),
                ..ListContainersOptions::default()
            })),
        )
        .await
        .unwrap_or_else(|error| {
            errors.push(Error::new(format!("list containers: {error}")));
            Vec::new()
        });
        for container in containers {
            let id = container.id.unwrap_or_default();
            if let Err(error) = self.remove_container(ctx, client, &id).await {
                errors.push(error);
            }
        }

        if self.resources.networks {
            let networks = with_timeout(
                ctx,
                self.op_timeout,
                client.list_networks(Some(ListNetworksOptions {
                    filters: Some(filters.clone()),
                })),
            )
            .await
            .unwrap_or_else(|error| {
                errors.push(Error::new(format!("list networks: {error}")));
                Vec::new()
            });
            for network in networks {
                let id = network.id.unwrap_or_default();
                if let Err(error) = self.remove_network(ctx, client, &id).await {
                    errors.push(error);
                }
            }
        }

        if self.resources.images {
            let images = with_timeout(
                ctx,
                self.op_timeout,
                client.list_images(Some(ListImagesOptions {
                    filters: Some(filters),
                    ..ListImagesOptions::default()
                })),
            )
            .await
            .unwrap_or_else(|error| {
                errors.push(Error::new(format!("list images: {error}")));
                Vec::new()
            });
            for image in images {
                if let Err(error) = self.remove_image(ctx, client, &image.id).await {
                    errors.push(error);
                }
            }
        }

        Error::join(errors).map_or(Ok(()), Err)
    }

    /// Force-removes the container `id`, with its anonymous volumes if configured; one already
    /// gone is not an error.
    ///
    /// # Errors
    ///
    /// The removal failed: `remove container <id>: …`.
    pub async fn remove_container(
        &self,
        ctx: &Context,
        client: &Docker,
        id: &str,
    ) -> Result<(), Error> {
        let removed = with_timeout(
            ctx,
            self.op_timeout,
            client.remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: self.resources.remove_volumes,
                    ..RemoveContainerOptions::default()
                }),
            ),
        )
        .await;
        match removed {
            Err(error) if !error.is_not_found() => {
                Err(Error::new(format!("remove container {id}: {error}")))
            }
            Ok(()) | Err(_) => Ok(()),
        }
    }

    /// Removes the network `id`; one already gone is not an error.
    ///
    /// # Errors
    ///
    /// The removal failed: `remove network <id>: …`.
    pub async fn remove_network(
        &self,
        ctx: &Context,
        client: &Docker,
        id: &str,
    ) -> Result<(), Error> {
        let removed = with_timeout(ctx, self.op_timeout, client.remove_network(id)).await;
        match removed {
            Err(error) if !error.is_not_found() => {
                Err(Error::new(format!("remove network {id}: {error}")))
            }
            Ok(()) | Err(_) => Ok(()),
        }
    }

    /// Force-removes the image `id`; one already gone is not an error.
    ///
    /// # Errors
    ///
    /// The removal failed: `remove image <id>: …`.
    pub async fn remove_image(
        &self,
        ctx: &Context,
        client: &Docker,
        id: &str,
    ) -> Result<(), Error> {
        let removed = with_timeout(
            ctx,
            self.op_timeout,
            client.remove_image(
                id,
                Some(RemoveImageOptions {
                    force: true,
                    ..RemoveImageOptions::default()
                }),
                None,
            ),
        )
        .await;
        match removed {
            Err(error) if !error.is_not_found() => {
                Err(Error::new(format!("remove image {id}: {error}")))
            }
            Ok(_) | Err(_) => Ok(()),
        }
    }
}

/// The sweep rules for this process: `now`, this host and PID namespace, the `stale_after`
/// window (the suite reads it with [`crate::sweep_rules::stale_after_from_env`]) and a PID check
/// on this host ([`process_alive`]).
#[must_use]
pub fn sweep_rules(
    now: Timestamp,
    stale_after: Duration,
    self_id: &str,
    owner: &Owner,
) -> SweepRules {
    SweepRules {
        now,
        stale_after,
        host: owner.host.clone(),
        pid_namespace: owner.pid_namespace.clone(),
        self_id: self_id.to_owned(),
        alive: Box::new(process_alive),
    }
}

/// Whether `pid` exists on this host: signal 0 checks without delivering, and a permission error
/// still means the process is there. A PID outside `pid_t` names no process: Linux keeps PIDs
/// under 2^22. Zero or a negative PID (which [`crate::sweep_rules::parse_owner`] refuses before
/// a sweep asks) names none either.
#[must_use]
pub fn process_alive(pid: i64) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .filter(|&pid| pid > 0)
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(errno) => errno == rustix::io::Errno::PERM,
    }
}

/// The label filter selecting resources that carry `value` (a key, or `key=value`).
fn label_filters(value: String) -> HashMap<String, Vec<String>> {
    HashMap::from([("label".to_owned(), vec![value])])
}

/// A Docker call that failed, or the context it ran under ended first.
#[derive(Debug)]
pub enum OpError {
    /// The call failed.
    Docker(DockerError),
    /// The context ended first.
    Ended(Ended),
}

impl OpError {
    /// containerd's `errdefs.IsNotFound`: the daemon answered 404.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::Docker(DockerError::DockerResponseServerError {
                status_code: 404,
                ..
            })
        )
    }
}

impl fmt::Display for OpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Docker(error) => error.fmt(formatter),
            Self::Ended(ended) => ended.fmt(formatter),
        }
    }
}

impl std::error::Error for OpError {}

impl From<DockerError> for OpError {
    fn from(error: DockerError) -> Self {
        Self::Docker(error)
    }
}

/// Runs one Docker call under its own deadline, `timeout` from now, derived from `ctx`.
///
/// # Errors
///
/// The call's error, or why the context ended first.
pub async fn with_timeout<T, E: Into<OpError>>(
    ctx: &Context,
    timeout: Duration,
    call: impl Future<Output = Result<T, E>>,
) -> Result<T, OpError> {
    ctx.with_timeout(timeout)
        .run(call)
        .await
        .map_err(OpError::Ended)?
        .map_err(Into::into)
}

#[cfg(test)]
mod tests;
