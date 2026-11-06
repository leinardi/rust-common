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

//! A Docker Engine API client on hyper's HTTP/1 client that sends what moby client v0.6.0 sends
//! and words its failures as moby words them: the transport, moby's lazy API version negotiation,
//! the event stream, and Go's `encoding/json` decoding of the answers. The calls a program makes,
//! and the types it decodes them into, stay the program's: it builds them on [`Client::get`],
//! [`Client::get_list`], [`Client::get_object`], [`Client::events`] and, with `write`,
//! [`Client::exchange`].
//!
//! - [`Settings`]: where the daemon is and how to talk to it. [`Settings::unix`] takes a socket
//!   path; with `tcp`, [`Settings::from_env`] reads `DOCKER_HOST`, `DOCKER_API_VERSION` and, with
//!   `tls`, `DOCKER_CERT_PATH` and `DOCKER_TLS_VERIFY`. The `User-Agent`
//!   ([`Settings::user_agent`], moby's own by default) and the response-header timeout
//!   ([`Settings::response_header_timeout`], none by default, as moby's) are the caller's.
//! - [`Client`] sends HTTP/1.1 only, with no proxy: unix sockets with the `api.moby.localhost`
//!   host, TCP (`tcp`) with or without TLS 1.2+ (`tls`), a 10s dial and a 10s TLS handshake
//!   timeout, up to two idle connections kept for 30s, and on TCP Go's `Accept-Encoding: gzip`
//!   with the transparent decompression that comes with it. A request that fails on a reused
//!   connection before any answer is sent again on another only when net/http would: a `GET` or
//!   `HEAD`, or any request hyper never wrote.
//! - The first versioned request pings `/_ping` (`HEAD`, then `GET`) and negotiates the API
//!   version down from 1.56 ([`version`]); a failed negotiation is not an error, exactly as in
//!   moby, and concurrent first requests share one ping. `DOCKER_API_VERSION` pins it.
//! - [`context`]: Go's `context.Context` for a call ([`with_context`]); a request it cuts short
//!   fails as net/http fails it, and [`with_context_timeout`] bounds one call that way.
//!   [`with_bare_timeout`] and [`with_bare_deadline`] bound a call the client does not see, with
//!   the bare `context deadline exceeded` (swarm-scheduler-exporter's `with_timeout` and
//!   `with_deadline`). swarm-device-access's own `with_timeout` is [`with_context_timeout`], not
//!   [`with_bare_timeout`]: the names differ so that neither port swaps one for the other unseen.
//! - [`events`]: moby's JSON stream decoder over a chunked body, each record bounded to 1 MiB.
//! - [`types`]: Go's `json.Decoder` on an answer ([`types::decode_first`],
//!   [`types::decode_list`]), with a JSON `null` as Go's zero value.
//!
//! Features: `tcp` (`DOCKER_HOST`, TCP, gzip, [`Settings::from_env`]), `tls` (implies `tcp`),
//! `write` ([`Client::exchange`]), `attach` (implies `write`: [`Client::exchange_upgrade`], moby's
//! [`stdcopy`] and the [`wait`] answer's parser) and `test-util` (the `test_util` fake engine,
//! request recorder and test helpers). Without any, the client speaks to a unix socket only.
//!
//! Extracted from swarm-scheduler-exporter/src/docker/mod.rs at c9e79afa8810 (the module map, the
//! TCP and TLS transport, the gzip, the response-header timeout).
//! Extracted from swarm-device-access/src/docker/mod.rs at 8b4ab3a3e417 (the context, the unix
//! socket settings, the writes and the attach).

mod client;
mod connect;
pub mod context;
mod error;
pub mod events;
mod host;
mod pool;
mod query;
#[cfg(feature = "attach")]
pub mod stdcopy;
#[cfg(feature = "test-util")]
pub mod test_util;
#[cfg(feature = "tls")]
mod tls;
pub mod types;
pub mod version;
#[cfg(feature = "attach")]
pub mod wait;

pub use client::{Body, Client, Settings, ensure_reader_closed, go_arch, moby_user_agent};
pub use context::{CallContext, ContextError, with_context, with_context_timeout};
pub use error::{ConfigError, Error, ErrorKind};
pub use events::{JsonStream, StreamError, StreamErrorKind};
pub use query::{
    Filters, encode_query, push_json_string, since_parameter, trim_id, with_bare_deadline,
    with_bare_timeout,
};
#[cfg(feature = "tls")]
pub use tls::PathError;
pub use version::{MAX_API_VERSION, MIN_API_VERSION, UnsupportedApiVersion};

use std::time::Duration;

/// moby's `DefaultDockerHost` on Unix: what [`Settings::from_env`] connects to without
/// `DOCKER_HOST`.
pub const DEFAULT_DOCKER_HOST: &str = "unix:///var/run/docker.sock";

/// moby's `DummyHost`: the `Host` header of requests over a unix socket.
pub const DUMMY_HOST: &str = "api.moby.localhost";

/// go-connections' dial timeout, unix and TCP alike.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// go-connections' TLS handshake timeout.
#[cfg(feature = "tls")]
pub const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The moby client release whose requests the client sends, as its default `User-Agent` names it.
pub const MOBY_CLIENT_VERSION: &str = "v0.6.0";

#[cfg(test)]
mod tests;
