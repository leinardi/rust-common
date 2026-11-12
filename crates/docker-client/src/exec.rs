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

//! A command run in a running container: created, started with its standard streams as one
//! bidirectional connection, its TTY resized, and inspected for its exit code.
//!
//! Every call goes over docker-client's own connection, not bollard's: bollard's exec start reads
//! a TTY session through its framing guess (a chunk starting with `\x01` is taken for a header),
//! its resize takes an `i32` where moby's Go client sends a `uint` (the engine reads a `u32`), and its create body leaves
//! out the fields moby's writes. The request bodies here are the ones moby's Go client writes,
//! byte for byte.
//!
//! The wire is moby/moby `client/container_exec.go` and `client/hijack.go`'s at client/v0.6.2
//! (`ExecCreate`, `ExecAttach`, `ExecResize`, `ExecInspect`, `setupHijackConn`); the code is
//! written anew, not extracted.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bollard::errors::Error as BollardError;
use http::Method;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::demux::Framing;
use crate::error::Error;
use crate::stream::{Call, Target, complete, failure, upgrade};

/// What to run, and which of its standard streams to attach.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent flags, each one field of the request moby writes"
)]
pub struct ExecOptions {
    /// The command and its arguments; none is sent as `null`, as Go sends a nil `Cmd`.
    pub cmd: Vec<String>,
    /// `NAME=value` entries added to the container's environment; none is sent as `null`.
    pub env: Vec<String>,
    /// Whether the command gets a TTY; its output is then raw, not multiplexed.
    pub tty: bool,
    /// Whether the session's writes reach the command's stdin.
    pub attach_stdin: bool,
    /// Whether the command's stdout is sent back.
    pub attach_stdout: bool,
    /// Whether the command's stderr is sent back.
    pub attach_stderr: bool,
}

impl ExecOptions {
    /// `cmd`, with no environment, no TTY and nothing attached.
    #[must_use]
    pub fn new(cmd: Vec<String>) -> Self {
        Self {
            cmd,
            ..Self::default()
        }
    }

    /// The framing of the command's output: [`Framing::Raw`] with a TTY, else
    /// [`Framing::Multiplexed`].
    #[must_use]
    pub const fn framing(&self) -> Framing {
        if self.tty {
            Framing::Raw
        } else {
            Framing::Multiplexed
        }
    }

    /// The body of `POST /containers/{id}/exec` as moby's Go client writes it: every field, no
    /// console size (the TTY is sized by [`resize`] after the start), `null` for no environment.
    fn body(&self) -> String {
        let env = go_json_or_null(&self.env);
        format!(
            r#"{{"User":"","Privileged":false,"Tty":{},"AttachStdin":{},"AttachStderr":{},"AttachStdout":{},"DetachKeys":"","Env":{env},"WorkingDir":"","Cmd":{}}}"#,
            self.tty,
            self.attach_stdin,
            self.attach_stderr,
            self.attach_stdout,
            go_json_or_null(&self.cmd),
        )
    }
}

/// `items` as [`go_json`] writes them, or `null` for none: Go's nil slice.
fn go_json_or_null(items: &[String]) -> String {
    if items.is_empty() {
        "null".to_owned()
    } else {
        go_json(items)
    }
}

/// `items` as a JSON array the way Go's `encoding/json` writes it: `<`, `>`, `&`, U+2028 and
/// U+2029 escaped.
fn go_json(items: &[String]) -> String {
    let mut written = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            written.push(',');
        }
        // A string always serializes.
        let quoted = serde_json::to_string(item).unwrap_or_default();
        for character in quoted.chars() {
            match character {
                '<' => written.push_str("\\u003c"),
                '>' => written.push_str("\\u003e"),
                '&' => written.push_str("\\u0026"),
                '\u{2028}' => written.push_str("\\u2028"),
                '\u{2029}' => written.push_str("\\u2029"),
                other => written.push(other),
            }
        }
    }
    written.push(']');
    written
}

/// `id` without surrounding whitespace, or moby's refusal of an empty one, as its client's `trimID`
/// does before `ExecCreate` and `ExecResize` (not before the start or the inspect).
fn trim_id<'id>(kind: &str, id: &'id str) -> Result<&'id str, Error> {
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::from(failure(format!(
            "invalid {kind} name or ID: value is empty"
        ))));
    }
    Ok(id)
}

/// Creates an exec instance of `options` in the running container `container` and returns its ID.
/// `timeout` bounds the whole call; `cancel` ends it at once.
///
/// # Errors
///
/// The engine's refusal sorted into its kind: [`ErrorKind::Conflict`](crate::ErrorKind::Conflict)
/// when the container is not running (moby's Go client reads a 409 there the same way),
/// `NotFound` when it is not there; an empty `container` (whitespace trimmed, as moby's client
/// does) is refused before sending and an answer without an `Id` is `Other`; the bound's `Timeout`,
/// `Cancelled`, and `Unavailable` when the engine cannot be reached.
pub async fn create(
    config: &Config,
    container: &str,
    options: &ExecOptions,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<String, Error> {
    let container = trim_id("container", container)?;
    let path = format!("/containers/{container}/exec");
    let call = Call::new(Method::POST, &path, "").json(options.body());
    let body = complete(config, &Target::of(config), call, timeout, cancel).await?;
    let answer: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| Error::from(BollardError::from(error)))?;
    answer
        .get("Id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::from(failure("the exec create answer has no Id".to_owned())))
}

/// Starts the exec instance `id` and returns its standard streams as one connection, once the
/// engine has taken it over (`101`). `framing` is the instance's: [`Framing::Raw`] for one
/// created with a TTY, [`Framing::Multiplexed`] otherwise ([`ExecOptions::framing`]); the start
/// body says `"Tty"` accordingly.
///
/// Writes reach the command's stdin and a shutdown of the write side closes it; reads are its
/// output, as the engine sends it: raw with a TTY, else multiplexed, for a
/// [`Demuxer`](crate::demux::Demuxer) of the same `framing`. `timeout` bounds the connection and
/// the engine's answer, not the session, which lasts until the command ends or the stream is
/// dropped; `cancel` ends the establishment at once.
///
/// # Errors
///
/// An answer other than `101` carries its status and moby's text, `unable to upgrade to tcp,
/// received <status>`: 404 (no such exec instance) is `NotFound`, 409 `Conflict`; the bound's
/// `Timeout`, `Cancelled`, and `Unavailable` when the engine cannot be reached.
pub async fn start(
    config: &Config,
    id: &str,
    framing: Framing,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<ExecStream, Error> {
    let path = format!("/exec/{id}/start");
    let tty = framing == Framing::Raw;
    let call =
        Call::new(Method::POST, &path, "").json(format!(r#"{{"Detach":false,"Tty":{tty}}}"#));
    let upgraded = upgrade(config, &Target::of(config), call, timeout, cancel).await?;
    Ok(ExecStream(TokioIo::new(upgraded)))
}

/// Sets the TTY of the exec instance `id` to `rows` by `cols`. `timeout` bounds the whole call;
/// `cancel` ends it at once.
///
/// # Errors
///
/// An empty `id` (whitespace trimmed, as moby's client does) is refused before sending, as
/// `Other`; the engine's refusal sorted into its kind (a command that has already exited is
/// refused), the
/// bound's `Timeout`, `Cancelled`, and `Unavailable` when the engine cannot be reached.
pub async fn resize(
    config: &Config,
    id: &str,
    rows: u32,
    cols: u32,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let id = trim_id("exec", id)?;
    let path = format!("/exec/{id}/resize");
    let query = format!("h={rows}&w={cols}");
    complete(
        config,
        &Target::of(config),
        Call::new(Method::POST, &path, &query),
        timeout,
        cancel,
    )
    .await
    .map(drop)
}

/// The state of an exec instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ExecState {
    /// Whether the command is still running.
    pub running: bool,
    /// Its exit code, once it has one; the engine sends none while it runs.
    pub exit_code: Option<i64>,
}

impl ExecState {
    /// A state, for a fake engine behind a consumer's own trait.
    #[must_use]
    pub const fn new(running: bool, exit_code: Option<i64>) -> Self {
        Self { running, exit_code }
    }
}

/// Inspects the exec instance `id`. A missing `Running` reads as not running and a missing or
/// `null` `ExitCode` as none, as moby's Go client decodes them. `timeout` bounds the whole call;
/// `cancel` ends it at once.
///
/// # Errors
///
/// The engine's refusal sorted into its kind (404 is `NotFound`), an answer that is not JSON or
/// whose `Running` or `ExitCode` has another type (`Other`), the bound's `Timeout`, `Cancelled`,
/// and `Unavailable` when the engine cannot be reached.
pub async fn inspect(
    config: &Config,
    id: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<ExecState, Error> {
    let path = format!("/exec/{id}/json");
    let body = complete(
        config,
        &Target::of(config),
        Call::new(Method::GET, &path, ""),
        timeout,
        cancel,
    )
    .await?;
    let answer: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| Error::from(BollardError::from(error)))?;
    let wrong = |field: &str| Error::from(failure(format!("the exec inspect {field} is mistyped")));
    let running = match answer.get("Running") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => value.as_bool().ok_or_else(|| wrong("Running"))?,
    };
    let exit_code = match answer.get("ExitCode") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_i64().ok_or_else(|| wrong("ExitCode"))?),
    };
    Ok(ExecState { running, exit_code })
}

/// A started exec instance's standard streams: reads are its output, writes its stdin. Dropping it
/// closes the connection.
pub struct ExecStream(TokioIo<Upgraded>);

impl std::fmt::Debug for ExecStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ExecStream { .. }")
    }
}

impl AsyncRead for ExecStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(context, buffer)
    }
}

impl AsyncWrite for ExecStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(context)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write_vectored(context, buffers)
    }

    fn is_write_vectored(&self) -> bool {
        self.0.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strings are escaped as Go's `encoding/json` escapes them: `json.Marshal` of an
    /// `ExecCreateRequest` with this `Cmd`, printed by a Go 1.26 program against moby api v1.56.1
    /// (DEL stays raw, `\b` and `\f` are short escapes, U+2028 and U+2029 are escaped).
    #[test]
    fn the_create_body_escapes_as_go_does() {
        let mut options = ExecOptions::new(vec![
            "echo \"hi\"\n".to_owned(),
            "\x01\x1f\x7f\t\x08\x0c\r".to_owned(),
            "\u{2028}\u{2029}é\\/".to_owned(),
        ]);
        options.env = Vec::new();
        assert!(
            ExecOptions::new(Vec::new())
                .body()
                .contains(r#""Cmd":null}"#)
        );
        assert_eq!(
            options.body(),
            "{\"User\":\"\",\"Privileged\":false,\"Tty\":false,\"AttachStdin\":false,\
             \"AttachStderr\":false,\"AttachStdout\":false,\"DetachKeys\":\"\",\"Env\":null,\
             \"WorkingDir\":\"\",\"Cmd\":[\"echo \\\"hi\\\"\\n\",\"\\u0001\\u001f\x7f\\t\\b\\f\\r\",\
             \"\\u2028\\u2029é\\\\/\"]}"
        );
    }
}
