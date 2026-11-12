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

//! A container's or a service's log stream, as frames of the framing the caller knows, followed by
//! exactly one end that says why the stream stopped.
//!
//! The requests are moby's Go client's (`ContainerLogs`, `ServiceLogs`), for the options runhold
//! uses (a container's `details` is not offered): a flag is sent as `=1`
//! only when set, `tail` only when it is neither empty nor `all` (the engine's default is all),
//! `since` and `until` only when not empty, the keys sorted and the values escaped as Go's
//! `url.Values.Encode` does, and the ID trimmed, an empty one refused before sending. moby's client
//! also turns a `since` or `until` given as a time or a duration into the engine's
//! `<seconds>.<nanoseconds>` form against the wall clock; that conversion is the caller's, and the
//! value is sent as given. A service's log stream has no `until`: the engine ignores it there. The
//! ID goes into the path as it is, where moby's client escapes it: an engine-issued ID or name
//! needs no escaping, one with a space is refused before sending, and one with `/`, `?`, `#` or
//! `%` is the caller's to keep out, as it would reach another endpoint.
//!
//! The ends are the classes runhold's Go client tells apart (`streamend.go`): the body ended
//! ([`StreamEnd::Eof`]; a partial frame at the end is dropped, as `stdcopy.StdCopy` does), the
//! caller's token fired ([`StreamEnd::Cancelled`]), or the transport or the stream failed
//! ([`StreamEnd::Failed`]: a connection cut mid-body, an error the engine sent in the stream, an
//! unknown stream byte).
//!
//! The wire is moby/moby `client/container_logs.go`, `client/service_logs.go` and
//! `client/utils.go`'s at client/v0.6.2; the code is written anew, not extracted.

use std::time::Duration;

use http::Method;
use http_body_util::BodyExt as _;
use hyper::body::Incoming;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::demux::{Demuxer, Frame, Framing};
use crate::error::Error;
use crate::stream::{Call, Target, establish, failure, trim_id};

/// What to ask of a container's log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags, each one query parameter moby sends"
)]
pub struct ContainerLogsOptions {
    /// `stdout=1`: the container's stdout.
    pub stdout: bool,
    /// `stderr=1`: the container's stderr.
    pub stderr: bool,
    /// `follow=1`: keep the stream open for new lines.
    pub follow: bool,
    /// `timestamps=1`: prefix each line with its time.
    pub timestamps: bool,
    /// `tail=<value>`: the number of lines from the end; empty or `all` sends nothing.
    pub tail: Option<String>,
    /// `since=<value>`, as the engine reads it (`<seconds>[.<nanoseconds>]`); empty sends nothing.
    pub since: Option<String>,
    /// `until=<value>`, as `since`.
    pub until: Option<String>,
}

/// What to ask of a service's log, aggregated across its tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags, each one query parameter moby sends"
)]
pub struct ServiceLogsOptions {
    /// `stdout=1`: the tasks' stdout.
    pub stdout: bool,
    /// `stderr=1`: the tasks' stderr.
    pub stderr: bool,
    /// `follow=1`: keep the stream open for new lines.
    pub follow: bool,
    /// `timestamps=1`: prefix each line with its time.
    pub timestamps: bool,
    /// `details=1`: prefix each line with its task's details (`com.docker.swarm.*=...`).
    pub details: bool,
    /// `tail=<value>`: the number of lines from the end; empty or `all` sends nothing.
    pub tail: Option<String>,
    /// `since=<value>`, as the engine reads it; empty sends nothing.
    pub since: Option<String>,
}

/// Why a log stream stopped.
#[derive(Debug)]
#[non_exhaustive]
pub enum StreamEnd {
    /// The engine ended the body.
    Eof,
    /// The caller's token fired.
    Cancelled,
    /// The connection or the stream failed; `Unavailable` for the transport, `Other` for an error
    /// in the stream.
    Failed(Error),
}

/// An item of a log stream: a frame, or the one end that follows the last.
#[derive(Debug)]
#[non_exhaustive]
pub enum LogItem {
    /// A piece of output.
    Frame(Frame),
    /// The end; nothing follows it.
    End(StreamEnd),
}

/// `GET /containers/{id}/logs` with `options`, read as `framing` (multiplexed unless the
/// container has a TTY). See [`service_logs`] for the stream, the bounds and the errors.
///
/// # Errors
///
/// As [`service_logs`]; a 404 is a container that is not there.
pub async fn container_logs(
    config: &Config,
    id: &str,
    options: &ContainerLogsOptions,
    framing: Framing,
    timeout: Duration,
    capacity: usize,
    cancel: CancellationToken,
) -> Result<mpsc::Receiver<LogItem>, Error> {
    let id = trim_id("container", id)?;
    let mut query = Query::default();
    query.flag("follow", options.follow);
    query.value("since", options.since.as_deref());
    query.flag("stderr", options.stderr);
    query.flag("stdout", options.stdout);
    query.tail(options.tail.as_deref());
    query.flag("timestamps", options.timestamps);
    query.value("until", options.until.as_deref());
    let path = format!("/containers/{id}/logs");
    logs(config, &path, &query.0, framing, timeout, capacity, cancel).await
}

/// `GET /services/{id}/logs` with `options`, read as `framing` (multiplexed unless the service's
/// container spec has a TTY), returned once the engine has answered 2xx.
///
/// A spawned task then sends each frame and, last, exactly one [`LogItem::End`]. It stops when
/// the receiver is dropped, even while the stream is idle (closing the connection). When `cancel`
/// fires it stops reading, even while the receiver is full, and sends [`StreamEnd::Cancelled`]
/// once there is room. A receiver closed with `close()` rather than dropped also stops the task,
/// so it may then end without an `End`. `timeout` bounds the connection and the headers, not the
/// stream; `capacity` is the channel's, clamped to at least one and at most tokio's
/// `Semaphore::MAX_PERMITS`. Must be called inside a tokio runtime.
///
/// # Errors
///
/// An empty `id` (whitespace trimmed, as moby's client does) is refused before sending, as
/// `Other`; [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when the headers do not come in
/// `timeout`, [`ErrorKind::Cancelled`](crate::ErrorKind::Cancelled) when `cancel` fires first, the
/// engine's refusal sorted into its kind (404 for a service that is not there, 500 `Other`),
/// `Unavailable` when it cannot be reached.
pub async fn service_logs(
    config: &Config,
    id: &str,
    options: &ServiceLogsOptions,
    framing: Framing,
    timeout: Duration,
    capacity: usize,
    cancel: CancellationToken,
) -> Result<mpsc::Receiver<LogItem>, Error> {
    let id = trim_id("service", id)?;
    let mut query = Query::default();
    query.flag("details", options.details);
    query.flag("follow", options.follow);
    query.value("since", options.since.as_deref());
    query.flag("stderr", options.stderr);
    query.flag("stdout", options.stdout);
    query.tail(options.tail.as_deref());
    query.flag("timestamps", options.timestamps);
    let path = format!("/services/{id}/logs");
    logs(config, &path, &query.0, framing, timeout, capacity, cancel).await
}

async fn logs(
    config: &Config,
    path: &str,
    query: &str,
    framing: Framing,
    timeout: Duration,
    capacity: usize,
    cancel: CancellationToken,
) -> Result<mpsc::Receiver<LogItem>, Error> {
    let target = Target::of(config);
    let body = establish(
        config,
        &target,
        Call::new(Method::GET, path, query),
        timeout,
        &cancel,
    )
    .await?;
    let (sender, receiver) = mpsc::channel(capacity.clamp(1, Semaphore::MAX_PERMITS));
    tokio::spawn(async move {
        let end = tokio::select! {
            biased;
            () = sender.closed() => return,
            end = pump(body, framing, &sender, &cancel) => end,
        };
        let _ = sender.send(LogItem::End(end)).await;
    });
    Ok(receiver)
}

/// Sends `body`'s frames until it ends, fails or `cancel` fires, and returns why it stopped.
async fn pump(
    mut body: Incoming,
    framing: Framing,
    sender: &mpsc::Sender<LogItem>,
    cancel: &CancellationToken,
) -> StreamEnd {
    let mut demuxer = Demuxer::new(framing);
    loop {
        loop {
            let frame = match demuxer.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => return StreamEnd::Failed(Error::from(failure(error.to_string()))),
            };
            let permit = tokio::select! {
                biased;
                () = cancel.cancelled() => return StreamEnd::Cancelled,
                permit = sender.reserve() => permit,
            };
            // A closed receiver ends the task in `logs`.
            let Ok(permit) = permit else {
                return StreamEnd::Eof;
            };
            permit.send(LogItem::Frame(frame));
        }
        let next = tokio::select! {
            biased;
            () = cancel.cancelled() => return StreamEnd::Cancelled,
            next = body.frame() => next,
        };
        match next {
            None => return StreamEnd::Eof,
            Some(Err(error)) => {
                return StreamEnd::Failed(Error::from(bollard::errors::Error::from(error)));
            }
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    demuxer.push(&data);
                }
            }
        }
    }
}

/// A query string written as Go's `url.Values.Encode` writes it; the keys are added in order.
#[derive(Default)]
struct Query(String);

impl Query {
    fn flag(&mut self, key: &str, set: bool) {
        if set {
            self.push(key, "1");
        }
    }

    fn value(&mut self, key: &str, value: Option<&str>) {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            self.push(key, value);
        }
    }

    fn tail(&mut self, value: Option<&str>) {
        self.value("tail", value.filter(|value| *value != "all"));
    }

    fn push(&mut self, key: &str, value: &str) {
        if !self.0.is_empty() {
            self.0.push('&');
        }
        self.0.push_str(key);
        self.0.push('=');
        query_escape(&mut self.0, value);
    }
}

/// Go's `url.QueryEscape`: letters, digits and `-_.~` as they are, a space as `+`, every other byte
/// as `%XX` in upper case.
fn query_escape(written: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                written.push(char::from(byte));
            }
            b' ' => written.push('+'),
            other => {
                written.push('%');
                written.push(char::from(HEX[usize::from(other >> 4)]));
                written.push(char::from(HEX[usize::from(other & 0x0f)]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go's `url.QueryEscape` of these values, from a Go 1.26 program.
    #[test]
    fn values_are_escaped_as_go_does() {
        let mut written = String::new();
        query_escape(&mut written, "a b*~-_.é/+%&=");
        assert_eq!(written, GO_ESCAPED);
    }

    const GO_ESCAPED: &str = "a+b%2A~-_.%C3%A9%2F%2B%25%26%3D";
}
