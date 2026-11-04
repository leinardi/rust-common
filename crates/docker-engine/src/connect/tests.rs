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

//! Dialing as Go's dialer: a unix socket's failures worded as Go's `net.OpError`, and a TCP
//! address's zone as the interface index Go dials.
//!
//! Extracted from swarm-scheduler-exporter/src/docker/connect/tests.rs at c9e79afa8810.
//! Extracted from swarm-device-access/src/docker/connect/tests.rs at 8b4ab3a3e417.

use super::{ConnectError, Connector};
use crate::host::Endpoint;

#[cfg(feature = "tcp")]
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

#[cfg(feature = "tcp")]
use super::scoped_ipv6;

fn unix_connector(socket: &str) -> Connector {
    #[cfg(feature = "tls")]
    return Connector::new(Endpoint::Unix(socket.to_owned()), None);
    #[cfg(not(feature = "tls"))]
    Connector::new(Endpoint::Unix(socket.to_owned()))
}

/// Go's `net.OpError` text of a socket that does not exist, which moby's errors quote: the dial
/// is reported with the path and the errno text.
#[tokio::test]
async fn a_missing_socket_is_a_dial_error_worded_as_go() {
    let directory =
        std::env::temp_dir().join(format!("docker-engine-connect-{}", std::process::id()));
    let socket = directory.join("missing.sock").display().to_string();
    let error = unix_connector(&socket)
        .connect()
        .await
        .expect_err("no socket there");
    assert!(matches!(error, ConnectError::Dial { .. }));
    assert_eq!(
        error.dial_error().and_then(std::io::Error::raw_os_error),
        Some(2)
    );
    assert!(!error.is_timeout());
    assert_eq!(
        error.to_string(),
        format!("dial unix {socket}: connect: no such file or directory")
    );
    let cause = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .expect("the io::Error");
    assert_eq!(cause.kind(), std::io::ErrorKind::NotFound);
}

/// A dial that runs out of time is Go's `i/o timeout`, a timeout as `net.Error` reports it.
#[test]
fn a_dial_timeout_reads_as_go() {
    let error = ConnectError::DialTimeout {
        network: "unix",
        address: "/var/run/docker.sock".to_owned(),
    };
    assert!(error.is_timeout());
    assert!(error.dial_error().is_none());
    assert!(std::error::Error::source(&error).is_none());
    assert_eq!(
        error.to_string(),
        "dial unix /var/run/docker.sock: i/o timeout"
    );
}

#[cfg(feature = "tcp")]
fn scoped(scope: u32) -> SocketAddr {
    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 2375, 0, scope))
}

#[cfg(feature = "tcp")]
#[test]
fn a_zone_becomes_the_interface_index_go_dials() {
    let loopback = std::fs::read_to_string("/sys/class/net/lo/ifindex")
        .ok()
        .and_then(|index| index.trim().parse().ok());
    if let Some(index) = loopback {
        assert_eq!(
            scoped_ipv6("::1%lo", 2375),
            Some(scoped(index)),
            "an interface name"
        );
    }
    assert_eq!(scoped_ipv6("::1%7", 2375), Some(scoped(7)), "a number");
    assert_eq!(
        scoped_ipv6("::1%no-such-interface", 2375),
        Some(scoped(0)),
        "an unknown name"
    );
    assert_eq!(scoped_ipv6("::1%..", 2375), Some(scoped(0)), "not a path");
    assert_eq!(scoped_ipv6("::1", 2375), None, "no zone: the resolver's");
    assert_eq!(scoped_ipv6("localhost%lo", 2375), None, "not a literal");
}
