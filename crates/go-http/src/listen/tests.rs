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

//! `parse_port` against Go's `LookupPort`, and `bind` on real loopback sockets against a Go 1.26
//! probe of `net.Listen`: `SO_REUSEADDR`, the error texts and the addresses they show, zones,
//! names, and the dual-stack wildcard. Only loopback and `localhost` are bound, and the one
//! name that must not resolve is under `.invalid`, so no test needs a network beyond the host.
//!
//! Extracted from swarm-device-access/src/server/tests.rs at 8b4ab3a3e417 (`ports_parse_as_go_s`).
//! Extracted from swarm-device-access/src/server/tests/shutdown.rs at 8b4ab3a3e417 (the bind
//! tests, which extend swarm-scheduler-exporter's).
//! Extracted from swarm-scheduler-exporter/src/server/tests/shutdown.rs at c9e79afa8810 (the
//! `http listen: ` prefix case).

use socket2::SockRef;
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;

use super::*;

/// Go's `LookupPort` answers, from a static Go program run without `/etc/services`.
#[test]
fn ports_parse_as_go_s() {
    assert_eq!(parse_port("8888"), Ok(8888));
    assert_eq!(parse_port("+80"), Ok(80));
    assert_eq!(
        parse_port("70000"),
        Err("address 70000: invalid port".to_owned())
    );
    assert_eq!(parse_port("-1"), Err("address -1: invalid port".to_owned()));
    assert_eq!(parse_port("http"), Ok(80));
    assert_eq!(parse_port("HTTP"), Ok(80));
    assert_eq!(parse_port("ssh"), Ok(22));
    assert_eq!(
        parse_port("domain"),
        Err("lookup tcp/domain: unknown port".to_owned())
    );
    assert_eq!(parse_port(""), Ok(0));
    assert_eq!(parse_port("+"), Ok(0));
    assert_eq!(parse_port("-0"), Ok(0));
    assert_eq!(parse_port("08"), Ok(8));
    assert_eq!(
        parse_port("0x10"),
        Err("lookup tcp/0x10: unknown port".to_owned())
    );
}

/// `SO_REUSEADDR` is on, read back with getsockopt: on the Go-style bind, and on tokio's own
/// bind too, which mio sets on Unix.
#[tokio::test]
async fn listeners_have_so_reuseaddr() {
    let tokio_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(SockRef::from(&tokio_listener).reuse_address().unwrap());
    let listener = bind("127.0.0.1:0", "").await.unwrap();
    assert!(SockRef::from(&listener).reuse_address().unwrap());
}

/// Whether a socket on local `port` is in `TIME_WAIT`, from `/proc/net/tcp`.
fn in_time_wait(port: u16) -> bool {
    let table = std::fs::read_to_string("/proc/net/tcp").unwrap();
    let local = format!(":{port:04X}");
    table.lines().skip(1).any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields
            .get(1)
            .is_some_and(|address| address.ends_with(&local))
            && fields.get(3) == Some(&"06")
    })
}

/// A restart rebinds while the old server-side connection is in `TIME_WAIT`; a socket without
/// `SO_REUSEADDR` cannot (EADDRINUSE).
#[tokio::test]
async fn a_rebind_succeeds_with_a_connection_in_time_wait() {
    let first = bind("127.0.0.1:0", "").await.unwrap();
    let address = first.local_addr().unwrap();
    // The server closes first, so its side of the connection goes to TIME_WAIT.
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink).await;
    });
    let (accepted, _) = first.accept().await.unwrap();
    drop(accepted);
    client.await.unwrap();
    drop(first);
    assert!(in_time_wait(address.port()), "no TIME_WAIT entry");

    let control = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
    let error = control.bind(&address.into()).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        Some(98),
        "EADDRINUSE without SO_REUSEADDR"
    );
    drop(bind(&address.to_string(), "").await.unwrap());
    drop(TcpListener::bind(address).await.unwrap());
}

/// A busy port, a missing port and a bad port fail as Go's `net.Listen` does, after the prefix
/// the caller chose; tokio's own bind cannot even read Go's `:port` (it resolves it, and fails in
/// the resolver's words).
#[tokio::test]
async fn a_bind_failure_is_go_s_error() {
    let busy = bind("127.0.0.1:0", "").await.unwrap();
    let busy_address = busy.local_addr().unwrap();
    for prefix in ["", "http listen: "] {
        let failed = bind(&busy_address.to_string(), prefix).await.unwrap_err();
        assert_eq!(
            failed.to_string(),
            format!("{prefix}listen tcp {busy_address}: bind: address already in use")
        );
        assert_eq!(
            failed.text(),
            format!("listen tcp {busy_address}: bind: address already in use")
        );
        assert_eq!(failed.prefix(), prefix);
        assert_eq!(
            bind("127.0.0.1", prefix).await.unwrap_err().to_string(),
            format!("{prefix}listen tcp: address 127.0.0.1: missing port in address")
        );
        assert_eq!(
            bind("127.0.0.1:70000", prefix)
                .await
                .unwrap_err()
                .to_string(),
            format!("{prefix}listen tcp: address 70000: invalid port")
        );
        assert_eq!(
            bind("127.0.0.1:nosuchsvc", prefix)
                .await
                .unwrap_err()
                .to_string(),
            format!("{prefix}listen tcp: lookup tcp/nosuchsvc: unknown port")
        );
    }
    assert!(TcpListener::bind(":8888").await.is_err());
}

/// The address Go's errors show for a busy port, by how the host was written, from a Go 1.26
/// probe of `net.Listen`: the wildcard literals all read `0.0.0.0`, an empty host `:port`, a
/// mapped address and a name their IPv4 address.
#[tokio::test]
async fn a_busy_port_is_shown_as_go_shows_it() {
    let busy = bind(":0", "").await.unwrap();
    let port = busy.local_addr().unwrap().port();
    for (host, shown) in [
        ("0.0.0.0", format!("0.0.0.0:{port}")),
        ("[::]", format!("0.0.0.0:{port}")),
        ("[::ffff:0.0.0.0]", format!("0.0.0.0:{port}")),
        ("", format!(":{port}")),
        ("127.0.0.1", format!("127.0.0.1:{port}")),
        ("[::ffff:127.0.0.1]", format!("127.0.0.1:{port}")),
        ("localhost", format!("127.0.0.1:{port}")),
    ] {
        assert_eq!(
            bind(&format!("{host}:{port}"), "")
                .await
                .unwrap_err()
                .to_string(),
            format!("listen tcp {shown}: bind: address already in use"),
            "{host}"
        );
    }
}

/// An IPv6 zone is an interface name, else a number, else 0, and is shown as given; a zone on an
/// IPv4 address makes it a name, which getaddrinfo refuses. From the same Go probe.
#[tokio::test]
async fn zones_are_go_s() {
    assert_eq!(zone_index("lo"), 1);
    assert_eq!(zone_index("12ab"), 12);
    assert_eq!(zone_index("99999999"), 0x00FF_FFFF);
    assert_eq!(zone_index("nosuchif"), 0);
    if !ipv6_available() {
        return;
    }
    let listener = bind("[::1%lo]:0", "").await.unwrap();
    assert_eq!(listener.local_addr().unwrap().ip().to_string(), "::1");
    for (address, cause) in [
        ("[fe80::1%lo]:0", "cannot assign requested address"),
        ("[fe80::1%nosuchif]:0", "invalid argument"),
        ("[fe80::1%999]:0", "no such device"),
    ] {
        assert_eq!(
            bind(address, "").await.unwrap_err().to_string(),
            format!("listen tcp {address}: bind: {cause}")
        );
    }
}

/// Go's resolver looks `inet_aton`'s shorthands up as names and finds nothing; getaddrinfo would
/// read `0` as `0.0.0.0` and bind the wildcard. A name that does not resolve has the resolver's
/// wording after Go's prefix.
#[tokio::test]
async fn names_resolve_as_go_s_resolver_does() {
    for host in ["0", "127.1", "0x7f.0.0.1", "010.0.0.1"] {
        assert_eq!(
            bind(&format!("{host}:0"), "")
                .await
                .unwrap_err()
                .to_string(),
            format!("listen tcp: lookup {host}: no such host")
        );
    }
    let error = bind("nonexistent.invalid:0", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("listen tcp: lookup nonexistent.invalid: "),
        "{error}"
    );
    let listener = bind("localhost:0", "").await.unwrap();
    assert_eq!(listener.local_addr().unwrap().ip().to_string(), "127.0.0.1");
}

/// The unspecified literals are the dual-stack wildcard too, as Go's `favoriteAddrFamily` makes
/// any wildcard; a mapped address listens as IPv4.
#[tokio::test]
async fn wildcard_literals_are_dual_stack() {
    for address in ["0.0.0.0:0", "[::]:0", "[::ffff:0.0.0.0]:0"] {
        let listener = bind(address, "").await.unwrap();
        let local = listener.local_addr().unwrap();
        assert!(local.ip().is_unspecified(), "{address}: {local}");
        if ipv6_available() {
            assert!(local.is_ipv6(), "{address}: {local}");
            assert!(!SockRef::from(&listener).only_v6().unwrap());
        }
    }
    let mapped = bind("[::ffff:127.0.0.1]:0", "").await.unwrap();
    assert_eq!(mapped.local_addr().unwrap().ip().to_string(), "127.0.0.1");
}

/// Whether the host can make IPv6 sockets at all.
fn ipv6_available() -> bool {
    Socket::new(Domain::IPV6, Type::STREAM, None).is_ok()
}

/// `:port` is the dual-stack wildcard, as Go's is: an IPv4 client reaches it.
#[tokio::test]
async fn an_empty_host_is_dual_stack() {
    let listener = bind(":0", "").await.unwrap();
    let local = listener.local_addr().unwrap();
    assert!(local.ip().is_unspecified(), "{local}");
    if ipv6_available() {
        assert!(local.is_ipv6(), "{local}");
        assert!(!SockRef::from(&listener).only_v6().unwrap());
    }
    let port = local.port();
    let accepted = tokio::spawn(async move { listener.accept().await.map(|(_, peer)| peer) });
    TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let peer = accepted.await.unwrap().unwrap();
    assert_eq!(peer.ip().to_canonical().to_string(), "127.0.0.1");
}
