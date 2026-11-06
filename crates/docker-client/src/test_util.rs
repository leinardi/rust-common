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

//! A fake Docker engine on a unix socket, for the tests of what talks to bollard.
//!
//! It answers each route with a scripted [`Reply`], records every request, and answers a route it
//! does not know with a 404. Every connection serves one request and closes, as bollard's client
//! (which keeps no idle connection) expects.
//!
//! Extracted from swarm-rootless-docker/src/docker/tests.rs at e0961f77655f (the engine that sends
//! the headers and stalls in the body), generalized to scripted routes.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

const JSON_HEAD: &str = "Content-Type: application/json\r\nConnection: close\r\n";

/// What a route answers.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A complete response with a JSON body.
    Json {
        /// The status code.
        status: u16,
        /// The body, sent whole with its length.
        body: String,
    },
    /// A `200` whose headers promise a body, `[` of which is sent before the connection is held
    /// open for an hour: bollard's own timeout (the headers) is met, a whole-call one is not.
    Stalled,
    /// These bytes as they are, then the connection closes.
    Raw(Vec<u8>),
}

impl Reply {
    /// A `200` with `body`.
    #[must_use]
    pub fn ok(body: &str) -> Self {
        Self::json(200, body)
    }

    /// An engine error: `status` with `{"message": message}`.
    #[must_use]
    pub fn error(status: u16, message: &str) -> Self {
        Self::json(status, &format!("{{\"message\":{message:?}}}"))
    }

    /// `status` with no body, as the engine answers `304` to a start of a running container.
    #[must_use]
    pub fn status(status: u16) -> Self {
        Self::json(status, "")
    }

    /// A `404`, as the engine answers for a container, image or network it does not have.
    #[must_use]
    pub fn not_found() -> Self {
        Self::error(404, "No such object")
    }

    /// A `409`, as the engine answers for a name that is taken.
    #[must_use]
    pub fn conflict() -> Self {
        Self::error(409, "Conflict. The name is already in use")
    }

    /// An event stream of one JSON `line` per event that closes after the last: the end of
    /// `/events`.
    #[must_use]
    pub fn events(lines: &[&str]) -> Self {
        let mut bytes = format!("HTTP/1.1 200 OK\r\n{JSON_HEAD}\r\n").into_bytes();
        for line in lines {
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
        Self::Raw(bytes)
    }

    /// An accepted attach: a `101` and then `stream`'s bytes, then the connection closes.
    #[must_use]
    pub fn upgrade(stream: &[u8]) -> Self {
        let mut bytes = b"HTTP/1.1 101 UPGRADED\r\nContent-Type: application/vnd.docker.raw-stream\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n".to_vec();
        bytes.extend_from_slice(stream);
        Self::Raw(bytes)
    }

    fn json(status: u16, body: &str) -> Self {
        Self::Json {
            status,
            body: body.to_owned(),
        }
    }
}

/// A request the engine received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The method.
    pub method: String,
    /// The target as sent, version prefix and query included.
    pub target: String,
    /// The headers as sent.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// The path without the `/v1.xx` version prefix and the query, which is what routes match.
    #[must_use]
    pub fn path(&self) -> &str {
        let path = self.target.split('?').next().unwrap_or(&self.target);
        if let Some(rest) = path.strip_prefix("/v")
            && let Some((version, tail)) = rest.split_once('/')
            && version
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
        {
            return &path[path.len() - tail.len() - 1..];
        }
        path
    }

    /// The query string without its `?`, as sent (percent-encoded); routes ignore it, so a test
    /// asserts the parameters here.
    #[must_use]
    pub fn query(&self) -> &str {
        self.target.split_once('?').map_or("", |(_, query)| query)
    }

    /// The first header called `name`, whatever its case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The fake engine: its socket lives in a scratch directory that is removed, and the task that
/// serves it stopped, when it is dropped.
#[derive(Debug)]
pub struct FakeEngine {
    dir: PathBuf,
    socket: PathBuf,
    requests: Arc<Mutex<Vec<Request>>>,
    task: JoinHandle<()>,
}

impl FakeEngine {
    /// Starts an engine that answers `routes`, `(path, reply)` pairs matched against
    /// [`Request::path`], the first match winning. Must be called inside a tokio runtime.
    ///
    /// # Errors
    ///
    /// The scratch directory or the socket cannot be made.
    pub fn start(routes: &[(&str, Reply)]) -> io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "docker-client-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        let socket = dir.join("docker.sock");
        let listener = UnixListener::bind(&socket)?;
        let routes: Arc<Vec<(String, Reply)>> = Arc::new(
            routes
                .iter()
                .map(|(path, reply)| ((*path).to_owned(), reply.clone()))
                .collect(),
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&routes), Arc::clone(&log)));
            }
        });
        Ok(Self {
            dir,
            socket,
            requests,
            task,
        })
    }

    /// The socket's path, for `Config::unix`.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Every request received so far, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn serve(
    mut stream: UnixStream,
    routes: Arc<Vec<(String, Reply)>>,
    log: Arc<Mutex<Vec<Request>>>,
) {
    let _ = answer(&mut stream, &routes, &log).await;
}

async fn answer(
    stream: &mut UnixStream,
    routes: &[(String, Reply)],
    log: &Mutex<Vec<Request>>,
) -> io::Result<()> {
    let request = read_request(stream).await?;
    let reply = routes
        .iter()
        .find(|(path, _)| path == request.path())
        .map_or_else(Reply::not_found, |(_, reply)| reply.clone());
    log.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(request);
    match reply {
        Reply::Json { status, body } => {
            let head = format!(
                "HTTP/1.1 {status} X\r\n{JSON_HEAD}Content-Length: {}\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(body.as_bytes()).await?;
        }
        Reply::Stalled => {
            let head = format!("HTTP/1.1 200 OK\r\n{JSON_HEAD}Content-Length: 1000\r\n\r\n[");
            stream.write_all(head.as_bytes()).await?;
            // Hold the connection open, the body unfinished.
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        Reply::Raw(bytes) => stream.write_all(&bytes).await?,
    }
    stream.shutdown().await
}

/// Reads the request line and headers, and drains a body its `Content-Length` announces.
async fn read_request(stream: &mut UnixStream) -> io::Result<Request> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split(' ');
    let request = Request {
        method: first.next().unwrap_or_default().to_owned(),
        target: first.next().unwrap_or_default().to_owned(),
        headers: lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
            .collect(),
    };
    let length: usize = request
        .header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut remaining = length.saturating_sub(buffer.len() - head_end);
    while remaining > 0 {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk[..remaining.min(4096)]).await?;
        if read == 0 {
            break;
        }
        remaining -= read;
    }
    Ok(request)
}
