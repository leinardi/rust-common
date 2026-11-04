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

//! A harness for Go wire captures: a request sent to a Go server and the bytes it answered,
//! replayed against a [`Server`] over an in-memory connection and compared byte for byte.
//!
//! A capture file holds a title line ending in `(port <n>)`, naming the Go server it was taken
//! from; a `### request: ` line with the request, `\r\n` written out; then the response as read,
//! followed by `[end: <nil>]` and an empty line once the server closed. A capture without that
//! end was cut before the response ended, and matches any response it is a prefix of. The
//! captures themselves stay in the programs whose Go servers they record.
//!
//! Extracted from swarm-scheduler-exporter/src/server/tests/wire.rs at c9e79afa8810 (the same as
//! swarm-device-access/src/server/tests/wire.rs at 8b4ab3a3e417): its parser, `Date` reader and
//! replay loop, with the routes and the list of expected differences left to the caller.

use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::{Server, Timeouts, connection};

/// What a capture's response ends with when the Go server closed the connection.
const END: &[u8] = b"[end: <nil>]\n\n";

/// One Go wire capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The capture's name: its file stem, for [`read_captures`].
    pub name: String,
    /// The port of the Go server it was taken from, which tells the caller how that server was
    /// set up.
    pub port: u16,
    /// The request as sent.
    pub request: Vec<u8>,
    /// The response as read.
    pub response: Vec<u8>,
    /// Whether the capture was cut before the response ended.
    pub truncated: bool,
}

impl Capture {
    /// Parses the capture `text`, named `name`.
    ///
    /// # Errors
    ///
    /// What is missing: the title's port, the request line or the response.
    pub fn parse(name: &str, text: &[u8]) -> Result<Self, String> {
        let mut lines = text.splitn(3, |byte| *byte == b'\n');
        let title = String::from_utf8_lossy(lines.next().unwrap_or_default()).into_owned();
        let port = title
            .rsplit_once("(port ")
            .and_then(|(_, port)| port.strip_suffix(')'))
            .and_then(|port| port.parse().ok())
            .ok_or_else(|| format!("{name}: no `(port <n>)` in the title {title:?}"))?;
        let request_line = String::from_utf8_lossy(lines.next().unwrap_or_default()).into_owned();
        let request = request_line
            .strip_prefix("### request: ")
            .ok_or_else(|| format!("{name}: no `### request: ` line"))?
            .replace("\\r\\n", "\r\n")
            .into_bytes();
        let rest = lines.next().ok_or_else(|| format!("{name}: no response"))?;
        let (response, truncated) = match rest.strip_suffix(END) {
            Some(response) => (response.to_vec(), false),
            None => (rest.strip_suffix(b"\n").unwrap_or(rest).to_vec(), true),
        };
        Ok(Self {
            name: name.to_owned(),
            port,
            request,
            response,
            truncated,
        })
    }

    /// The time of the response's first `Date`, so a server whose clock reads it answers the
    /// same `Date`; `None` without one.
    #[must_use]
    pub fn date(&self) -> Option<Timestamp> {
        let text = String::from_utf8_lossy(&self.response);
        let date = text
            .split("\r\n")
            .find_map(|line| line.strip_prefix("Date: "))?;
        jiff::fmt::strtime::parse("%a, %d %b %Y %H:%M:%S GMT", date)
            .and_then(|parsed| parsed.to_datetime())
            .and_then(|civil| civil.to_zoned(jiff::tz::TimeZone::UTC))
            .ok()
            .map(|zoned| zoned.timestamp())
    }

    /// Whether `response` is the captured one: the same bytes, or, for a capture cut before the
    /// end, bytes that start with it.
    #[must_use]
    pub fn matches(&self, response: &[u8]) -> bool {
        if self.truncated {
            response.starts_with(&self.response)
        } else {
            response == self.response.as_slice()
        }
    }
}

/// The head of `response`, up to the empty line that ends it (excluded); `None` before one.
#[must_use]
pub fn head(response: &[u8]) -> Option<&[u8]> {
    response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|end| &response[..end])
}

/// Every capture in `directory`, sorted by file name, each named by its file stem.
///
/// # Errors
///
/// The directory or a file cannot be read, or a file is not a capture
/// ([`io::ErrorKind::InvalidData`]).
pub fn read_captures(directory: &Path) -> io::Result<Vec<Capture>> {
    let mut paths = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = std::fs::read(path)?;
            Capture::parse(&name, &text)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        })
        .collect()
}

/// Sends `request` on a fresh in-memory connection to `server` and returns everything it wrote
/// before it closed.
pub async fn exchange(server: Arc<Server>, timeouts: Timeouts, request: &[u8]) -> Vec<u8> {
    let (mut client, server_io) = tokio::io::duplex(1 << 20);
    let serving = connection(server_io, server, timeouts);
    tokio::spawn(async move {
        let _ = serving.await;
    });
    let _ = client.write_all(request).await;
    let mut response = Vec::new();
    let _ = client.read_to_end(&mut response).await;
    response
}

/// How a replay went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Replay {
    /// The captures answered as Go answered them, by name, in replay order.
    pub identical: Vec<String>,
    /// The others, by name, in replay order.
    pub different: Vec<String>,
    /// Go's and the server's answer to each of the others.
    pub report: String,
}

/// Replays each of `captures` against the server `server_for` sets up for it (by its port, at
/// its [`Capture::date`]), on a fresh connection under `timeouts`.
pub async fn replay(
    captures: &[Capture],
    timeouts: Timeouts,
    mut server_for: impl FnMut(&Capture) -> Server,
) -> Replay {
    let mut replay = Replay::default();
    for capture in captures {
        let response = exchange(Arc::new(server_for(capture)), timeouts, &capture.request).await;
        if capture.matches(&response) {
            replay.identical.push(capture.name.clone());
        } else {
            let _ = writeln!(
                replay.report,
                "{} differs:\n--- Go\n{}\n--- Rust\n{}",
                capture.name,
                String::from_utf8_lossy(&capture.response),
                String::from_utf8_lossy(&response)
            );
            replay.different.push(capture.name.clone());
        }
    }
    replay
}
