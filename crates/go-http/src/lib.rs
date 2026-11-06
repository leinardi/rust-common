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

//! Go's `net/http` server on hyper, as the Go programs these crates replace served HTTP: a
//! `ServeMux` of exact paths behind an `http.Server`, answered byte for byte where hyper can.
//!
//! - Routing ([`Mux`], [`gopath`]): exact one-segment paths, any method; `OPTIONS *` as Go's
//!   global options handler; `CONNECT` and every other path Go's 404; a 307 to the cleaned path
//!   for an unclean one, keeping the query; a 400 for a path `url.ParseRequestURI` refuses.
//! - Answers ([`Reply`]): Go's header case and order, the handler's headers sorted by name, then
//!   `Date`, then `Content-Length`, then `Connection`; a sniffed `Content-Type` after
//!   `Content-Length` ([`Reply::sniffed`]). hyper keeps a header set by hand where it is and only
//!   appends what is missing, so all of them are set here; hyper's own `Date` is off, so its
//!   parse-error answers carry none, as Go's do.
//! - Framing: a body of at most 2 KiB (Go's `bufferBeforeChunkingSize`) is sent with a
//!   `Content-Length`; a larger one is chunked, in chunks of 2 KiB (Go's chunk boundaries follow
//!   the handler's writes, which no client sees), and a HEAD answer to it has neither header.
//! - Go's four server timeouts ([`timeouts`]), kept by an I/O wrapper instead of hyper's header
//!   timer. The caller passes its [`Timeouts`]: there is no default.
//! - The listener, bound as Go's `net.Listen` binds it ([`bind`], [`parse_port`]), and the accept
//!   loop with Go's retry and a drain that stops at the deadline it is handed ([`serve`]).
//!
//! What is the program's stays the program's: the routes and their handlers (a `/metrics`
//! handler builds its body with the program's registry, and [`MetricsRequest`] and
//! [`Reply::metrics`] carry promhttp's request and answer), the clock `Date` is read from, the log
//! callback that receives `net/http`'s own messages ([`log_printf_line`] gives them Go's
//! `log.Printf` prefix), the [`ListenError`] prefix, and what a server error or a late drain
//! means for the program.
//!
//! With the `testing` feature, the `testing` module replays Go wire captures against a
//! [`Server`].

pub mod gopath;
mod listen;
mod server;
pub mod timeouts;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use listen::{ListenError, bind, parse_port};
pub use server::{
    Drained, Handler, LogFn, MetricsRequest, Mux, Reply, ReplyFuture, ResponseBody, Served, Server,
    TEXT_PLAIN, connection, log_printf_line, serve,
};
pub use timeouts::{ConnState, TimedIo, Timeouts};
