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

//! Go's `net.Listen("tcp", address)` for a server: [`bind`], with `LookupPort`'s port names
//! ([`parse_port`]), the dual-stack wildcard, IPv6 zones and Go's error texts.
//!
//! Extracted from swarm-device-access/src/server.rs at 8b4ab3a3e417 (`bind`, `parse_port` with
//! Go's built-in TCP services, the wildcard literals, zones and `inet_aton`'s shorthands), which
//! extends swarm-scheduler-exporter/src/server.rs at c9e79afa8810; the `ListenError` prefix,
//! which only swarm-scheduler-exporter wrote, is the caller's.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

use go_compat::errno::io_error_text;
use go_compat::host_port::split_host_port;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;

/// A listen failure: Go's `net.OpError` text, after the prefix [`bind`] was given (`""` for
/// the bare text, `"http listen: "` for a program that wrapped it so).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenError {
    prefix: &'static str,
    text: String,
}

impl ListenError {
    /// Go's `net.OpError` text, without the prefix.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The prefix [`Display`](fmt::Display) writes before the text.
    #[must_use]
    pub fn prefix(&self) -> &'static str {
        self.prefix
    }
}

impl fmt::Display for ListenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{}", self.prefix, self.text)
    }
}

impl std::error::Error for ListenError {}

/// Go's built-in `services` table for TCP.
const GO_TCP_SERVICES: [(&str, u16); 14] = [
    ("ftp", 21),
    ("ftps", 990),
    ("gopher", 70),
    ("http", 80),
    ("https", 443),
    ("imap2", 143),
    ("imap3", 220),
    ("imaps", 993),
    ("pop3", 110),
    ("pop3s", 995),
    ("smtp", 25),
    ("submissions", 465),
    ("ssh", 22),
    ("telnet", 23),
];

/// Go's `LookupPort` for TCP: decimal digits with an optional sign, from 0 to 65535, or one of
/// the service names of Go's built-in `services` table, matched without regard to case. Go reads
/// `/etc/services` on top of that table; the programs these crates serve run in images without
/// one, so the table alone is what Go answers there.
///
/// # Errors
///
/// Go's text: `lookup tcp/<port>: unknown port` for a name, `address <port>: invalid port` for a
/// number out of range.
pub fn parse_port(port: &str) -> Result<u16, String> {
    let (negative, digits) = match port.as_bytes().first() {
        Some(b'+') => (false, &port[1..]),
        Some(b'-') => (true, &port[1..]),
        _ => (false, port),
    };
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return GO_TCP_SERVICES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(port))
            .map(|(_, number)| *number)
            .ok_or_else(|| format!("lookup tcp/{port}: unknown port"));
    }
    let value = digits.bytes().fold(0_u32, |value, digit| {
        value
            .saturating_mul(10)
            .saturating_add(u32::from(digit - b'0'))
    });
    match u16::try_from(value) {
        Ok(0) => Ok(0),
        Ok(value) if !negative => Ok(value),
        Ok(_) | Err(_) => Err(format!("address {port}: invalid port")),
    }
}

/// `net.Listen("tcp", address)`, which `TcpListener::bind` is not: an empty or unspecified host
/// (`0.0.0.0`, `[::]`, `[::ffff:0.0.0.0]`) is the dual-stack wildcard `[::]` (`0.0.0.0` only
/// where IPv6 is missing), an IPv4-mapped address listens as IPv4, an IPv6 zone is an interface
/// name or number, a host name listens on its first IPv4 address, `SO_REUSEADDR` is on, and the
/// backlog is the kernel's limit, as Go reads `somaxconn`.
///
/// # Errors
///
/// A [`ListenError`] with `error_prefix` and Go's wording for the address errors and the failing
/// system call; a host name that does not resolve has the resolver's wording after Go's
/// `listen tcp: lookup <host>: `.
pub async fn bind(address: &str, error_prefix: &'static str) -> Result<TcpListener, ListenError> {
    bind_text(address).await.map_err(|text| ListenError {
        prefix: error_prefix,
        text,
    })
}

/// [`bind`], failing with the bare text.
async fn bind_text(address: &str) -> Result<TcpListener, String> {
    let (host, port) = split_host_port(address).map_err(|error| format!("listen tcp: {error}"))?;
    let port = parse_port(port).map_err(|error| format!("listen tcp: {error}"))?;
    let target = match literal(host, port) {
        Some(target) => target,
        None => resolve(host, port).await?,
    };
    let (candidates, shown, dual_stack) = match target {
        Target::Wildcard { shown } => (
            vec![
                SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
            ],
            shown,
            true,
        ),
        Target::One { address, shown } => (vec![address], shown, false),
    };
    let mut last = format!("listen tcp {shown}: no suitable address");
    for candidate in candidates {
        match listen_on(candidate, dual_stack) {
            Ok(listener) => return Ok(listener),
            Err((call, error)) => {
                last = format!("listen tcp {shown}: {call}: {}", io_error_text(&error));
                // Only a host without IPv6 (EAFNOSUPPORT) falls back to 0.0.0.0: an address in
                // use on [::] must not quietly bind IPv4 only.
                if error.raw_os_error() != Some(97) {
                    return Err(last);
                }
            }
        }
    }
    Err(last)
}

/// Where [`bind`] listens, and the address Go's errors show for it.
enum Target {
    /// Go's wildcard: `[::]` with IPv4 mapped, else `0.0.0.0`.
    Wildcard { shown: String },
    /// One address.
    One { address: SocketAddr, shown: String },
}

/// Go's target for an IP: the wildcard for an unspecified one (shown `0.0.0.0`, as Go shows it
/// for `[::]` too), IPv4 for a mapped one, and the zone for IPv6, shown as given.
fn target_of(ip: IpAddr, zone: Option<&str>, port: u16) -> Target {
    let ip = ip.to_canonical();
    if ip.is_unspecified() {
        return Target::Wildcard {
            shown: format!("0.0.0.0:{port}"),
        };
    }
    match ip {
        IpAddr::V4(_) => Target::One {
            address: SocketAddr::new(ip, port),
            shown: format!("{ip}:{port}"),
        },
        IpAddr::V6(ipv6) => {
            let scope = zone.map_or(0, zone_index);
            let shown = match zone {
                Some(zone) => format!("[{ipv6}%{zone}]:{port}"),
                None => format!("[{ipv6}]:{port}"),
            };
            Target::One {
                address: SocketAddr::V6(SocketAddrV6::new(ipv6, port, 0, scope)),
                shown,
            }
        }
    }
}

/// The target of an empty host or an IP literal, which Go never resolves; `None` for a name.
/// A zone is IPv6's only, so `127.0.0.1%lo` is a name, as Go's `ParseIP` makes it.
fn literal(host: &str, port: u16) -> Option<Target> {
    if host.is_empty() {
        return Some(Target::Wildcard {
            shown: format!(":{port}"),
        });
    }
    let (text, zone) = match host.split_once('%') {
        Some((text, zone)) => (text, Some(zone)),
        None => (host, None),
    };
    match text.parse::<IpAddr>() {
        Ok(ip @ IpAddr::V6(_)) => Some(target_of(ip, zone, port)),
        Ok(ip @ IpAddr::V4(_)) if zone.is_none() => Some(target_of(ip, None, port)),
        Ok(IpAddr::V4(_)) | Err(_) => None,
    }
}

/// Go's resolution of a host name for `Listen`: its first IPv4 address, else its first.
async fn resolve(host: &str, port: u16) -> Result<Target, String> {
    // getaddrinfo reads `inet_aton`'s shorthands (`0`, `127.1`, `0x7f.1`) as addresses; Go's own
    // resolver looks them up as names, which no DNS server answers.
    if inet_aton_form(host) {
        return Err(format!("listen tcp: lookup {host}: no such host"));
    }
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("listen tcp: lookup {host}: {error}"))?
        .collect();
    resolved
        .iter()
        .find(|address| address.is_ipv4())
        .or_else(|| resolved.first())
        .map(|address| target_of(address.ip(), None, port))
        .ok_or_else(|| format!("listen tcp: lookup {host}: no such host"))
}

/// Whether `host` has `inet_aton`'s numeric shape: one to four dot-separated parts, each decimal,
/// `0`-prefixed octal or `0x`-prefixed hexadecimal.
fn inet_aton_form(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() <= 4
        && parts.iter().all(|part| {
            match part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                Some(hex) => hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
                None => !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()),
            }
        })
}

/// Go's cap on a zone's number (`big` in its `dtoi`).
const ZONE_INDEX_CAP: u32 = 0x00FF_FFFF;

/// Go's `zoneCache.index`: the interface of that name, else the zone's leading decimal digits
/// (capped as Go's `dtoi` caps them), else 0.
fn zone_index(zone: &str) -> u32 {
    let named = rustix::net::socket(
        rustix::net::AddressFamily::INET,
        rustix::net::SocketType::DGRAM,
        None,
    )
    .and_then(|socket| rustix::net::netdevice::name_to_index(&socket, zone));
    if let Ok(index) = named {
        return index;
    }
    let mut index = 0_u32;
    for byte in zone.bytes().take_while(u8::is_ascii_digit) {
        index = index * 10 + u32::from(byte - b'0');
        if index >= ZONE_INDEX_CAP {
            return ZONE_INDEX_CAP;
        }
    }
    index
}

/// One listening socket on `address`, with the system call that failed.
fn listen_on(
    address: SocketAddr,
    dual_stack: bool,
) -> Result<TcpListener, (&'static str, io::Error)> {
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )
    .map_err(|error| ("socket", error))?;
    socket
        .set_reuse_address(true)
        .map_err(|error| ("setsockopt", error))?;
    if dual_stack && address.is_ipv6() {
        socket
            .set_only_v6(false)
            .map_err(|error| ("setsockopt", error))?;
    }
    socket
        .set_nonblocking(true)
        .map_err(|error| ("setsockopt", error))?;
    socket
        .bind(&address.into())
        .map_err(|error| ("bind", error))?;
    // The kernel caps it at somaxconn, which is what Go asks for.
    socket
        .listen(i32::from(u16::MAX))
        .map_err(|error| ("listen", error))?;
    TcpListener::from_std(socket.into()).map_err(|error| ("listen", error))
}

#[cfg(test)]
mod tests;
