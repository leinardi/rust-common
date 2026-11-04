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

//! Ports of the Go standard library's formatting and parsing, for a Rust program that must match a
//! Go program's output byte for byte: its log lines, its flags, its metrics exposition, its error
//! texts. Each one is a port rather than a Rust equivalent, because the Rust equivalents differ in
//! the details (float ties, `+00:00` for UTC, Unicode versions, POSIX `TZ` strings, sort
//! stability, errno wording). The versions ported are go1.26's; the Unicode tables are Go's
//! Unicode 15.
//!
//! - [`float`] ports `strconv.FormatFloat(f, 'g', -1, 64)` (`%v`) and encoding/json's float
//!   layout;
//! - [`duration`] ports `time.Duration.String` and `time.ParseDuration`;
//! - [`quote`] ports `strconv.Quote` and `strconv.QuoteRune`, with Go's `IsPrint` and `IsSpace`;
//! - [`time_layout`] ports the `time.RFC3339Nano` and `2006-01-02T15:04:05.000Z07:00` layouts, and
//!   `time.Parse` of the first;
//! - [`local_zone`] ports how Go's `time.Local` is resolved from `TZ` and the zoneinfo files;
//! - [`strings`] ports `strings.EqualFold`, `strings.ToLower`, `strings.TrimSpace` and
//!   `strings.Fields`, with Go's case and space tables;
//! - [`parse_bool`] ports `strconv.ParseBool`;
//! - [`parse_int`] ports `strconv.ParseInt`, `strconv.ParseUint` and `strconv.Atoi`;
//! - [`parse_float`] ports `strconv.ParseFloat(s, 32)` and `strconv`'s `underscoreOK`;
//! - [`errno`] gives an `io::Error`, or an errno, Go's `syscall.Errno` text;
//! - [`errors`] ports `errors.Join`'s text;
//! - [`filepath`] ports `filepath.Match`, `filepath.Clean`, `filepath.Join` and `filepath.Base`,
//!   which on Unix are also `path.Clean` and `path.Join`;
//! - [`scanner`] ports `bufio.Scanner` with `ScanLines`;
//! - [`sort`] ports `sort.Sort`, whose order matters when the comparison is not a total order;
//! - [`http_status`] ports `net/http.StatusText`;
//! - [`net_ip`] ports `net.ParseIP`, `net.ParseCIDR`, `IP.Equal` and `IPNet.Contains`;
//! - [`host_port`] ports `net.SplitHostPort` and `net.JoinHostPort`.
//!
//! The ports were checked against Go programs: the unit tests carry Go's answers, and
//! `tests/go_compat_golden.rs` replays the corpora Go printed (`tests/testdata`).
//!
//! Extracted from swarm-device-access/src/go_compat/mod.rs at 8b4ab3a3e417, with `net_ip` from
//! swarm-scheduler-exporter/src/go_compat/net_ip.rs at c9e79afa8810.

pub mod duration;
pub mod errno;
pub mod errors;
pub mod filepath;
pub mod float;
pub mod host_port;
pub mod http_status;
pub mod local_zone;
pub mod net_ip;
pub mod parse_bool;
pub mod parse_float;
pub mod parse_int;
pub mod quote;
pub mod scanner;
pub mod sort;
pub mod strings;
pub mod time_layout;
mod unicode_case;
mod unicode_tables;
