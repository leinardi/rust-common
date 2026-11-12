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
use tokio::task::{JoinHandle, JoinSet};

const JSON_HEAD: &str = "Content-Type: application/json\r\nConnection: close\r\n";
/// The engine's answer to an attach or an exec start it accepts.
const UPGRADED: &[u8] = b"HTTP/1.1 101 UPGRADED\r\nContent-Type: application/vnd.docker.raw-stream\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n";

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
    /// The request is logged at once, `reply` is sent only after `delay`: an engine that is slow
    /// to answer the headers.
    Delayed {
        /// How long to wait before answering.
        delay: Duration,
        /// The answer.
        reply: Box<Reply>,
    },
    /// The request is logged and never answered, the connection held open.
    Silent,
    /// An accepted exec start or attach: a `101`, then `output` at once, then every byte the client
    /// writes is recorded ([`FakeEngine::sessions`]), and echoed back when `echo` is set, until
    /// the client shuts its write side down; then the connection closes. The connection stays
    /// open after `output` until the client's end, so a client that waits for more output than
    /// `output` before writing hangs: the test of a client that buffers.
    Session {
        /// What the command writes before reading anything.
        output: Vec<u8>,
        /// Whether each chunk the client writes is sent back.
        echo: bool,
    },
    /// A `200` with the headers sent at once and `body` only after `body_delay`: a wait the engine
    /// has registered but whose container has not exited yet.
    HeadersThenBody {
        /// How long the body is held back.
        body_delay: Duration,
        /// The body, sent whole with its length.
        body: String,
    },
}

impl Reply {
    /// A `200` with `body`.
    #[must_use]
    pub fn ok(body: &str) -> Self {
        Self::json(200, body)
    }

    /// `status` with a JSON `body`, as the engine answers `201` to a create.
    #[must_use]
    pub fn json_status(status: u16, body: &str) -> Self {
        Self::json(status, body)
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
        let mut bytes = UPGRADED.to_vec();
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
    /// The body as sent, as long as its `Content-Length` says (empty without one).
    pub body: Vec<u8>,
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

    /// The target as sent: the `/v1.xx` prefix (when the client sent one) and the query included,
    /// which is what an assertion on the API version reads.
    #[must_use]
    pub fn raw_path(&self) -> &str {
        &self.target
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
/// serves it stopped, and with it every connection it is serving, when it is dropped.
#[derive(Debug)]
pub struct FakeEngine {
    dir: PathBuf,
    socket: PathBuf,
    requests: Arc<Mutex<Vec<Request>>>,
    sessions: Arc<Mutex<Vec<Vec<u8>>>>,
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
        let sessions = Arc::new(Mutex::new(Vec::new()));
        let log = Log {
            requests: Arc::clone(&requests),
            sessions: Arc::clone(&sessions),
        };
        let task = tokio::spawn(async move {
            // The connection tasks live in this set, so aborting this task (the engine's drop)
            // drops the set and aborts them too, closing their sockets.
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        connections.spawn(serve(stream, Arc::clone(&routes), log.clone()));
                    }
                    Some(_) = connections.join_next() => {}
                }
            }
        });
        Ok(Self {
            dir,
            socket,
            requests,
            sessions,
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

    /// What the client wrote in each [`Reply::Session`] that has ended (the client shut its write
    /// side down), oldest first.
    #[must_use]
    pub fn sessions(&self) -> Vec<Vec<u8>> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Where the connections record what they received.
#[derive(Clone)]
struct Log {
    requests: Arc<Mutex<Vec<Request>>>,
    sessions: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn serve(mut stream: UnixStream, routes: Arc<Vec<(String, Reply)>>, log: Log) {
    let _ = answer(&mut stream, &routes, &log).await;
}

async fn answer(stream: &mut UnixStream, routes: &[(String, Reply)], log: &Log) -> io::Result<()> {
    let request = read_request(stream).await?;
    let reply = routes
        .iter()
        .find(|(path, _)| path == request.path())
        .map_or_else(Reply::not_found, |(_, reply)| reply.clone());
    log.requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(request);
    let mut reply = reply;
    while let Reply::Delayed { delay, reply: next } = reply {
        tokio::time::sleep(delay).await;
        reply = *next;
    }
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
        Reply::Silent => tokio::time::sleep(Duration::from_secs(3600)).await,
        Reply::Session { output, echo } => {
            stream.write_all(UPGRADED).await?;
            stream.write_all(&output).await?;
            stream.flush().await?;
            let mut received = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await?;
                if read == 0 {
                    break;
                }
                received.extend_from_slice(&chunk[..read]);
                if echo {
                    stream.write_all(&chunk[..read]).await?;
                    stream.flush().await?;
                }
            }
            log.sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(received);
        }
        Reply::HeadersThenBody { body_delay, body } => {
            let head = format!(
                "HTTP/1.1 200 OK\r\n{JSON_HEAD}Content-Length: {}\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await?;
            stream.flush().await?;
            tokio::time::sleep(body_delay).await;
            stream.write_all(body.as_bytes()).await?;
        }
        Reply::Delayed { .. } => unreachable!("flattened above"),
    }
    stream.shutdown().await
}

/// Reads the request line and headers, and the body its `Content-Length` announces.
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
    let mut request = Request {
        method: first.next().unwrap_or_default().to_owned(),
        target: first.next().unwrap_or_default().to_owned(),
        headers: lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
            .collect(),
        body: Vec::new(),
    };
    let length: usize = request
        .header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end..].to_vec();
    body.truncate(length);
    while body.len() < length {
        let mut chunk = [0_u8; 4096];
        let wanted = (length - body.len()).min(chunk.len());
        let read = stream.read(&mut chunk[..wanted]).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    request.body = body;
    Ok(request)
}
