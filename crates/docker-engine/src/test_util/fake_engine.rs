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

//! The core of a fake Docker Engine: a listener on TCP or a unix socket, each connection handed
//! to the test's [`Responder`], and just enough HTTP/1.1 for the client: request heads, request
//! bodies with a `Content-Length`, keep-alive, `Content-Length` answers, and chunked answers that
//! end with the terminating chunk, stay open until the client goes away, or are cut. Every request
//! read is recorded, in arrival order. What is answered (fixtures, captures, scripted event
//! streams, holds) is the test's.
//!
//! Extracted from swarm-scheduler-exporter/tests/support/fake_engine.rs at c9e79afa8810 (the TCP
//! fake: `read_head`, `respond`, the chunked event stream and its ends).
//! Extracted from swarm-device-access/tests/support/fake_engine.rs at 8b4ab3a3e417 (the unix
//! fake: `read_request` with its body, connection numbers, `canonical_key`).

use std::fmt::Write as _;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, UnixListener};

/// One request as it went over the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The client connection it came on, numbered from 1 in order of acceptance.
    pub conn: usize,
    /// The method.
    pub method: String,
    /// The request target, as sent (`/v1.56/events?since=…`).
    pub target: String,
    /// The headers, names as sent (Go's canonical case from the client), values trimmed, in the
    /// order the lines came.
    pub headers: Vec<(String, String)>,
    /// The body, empty when there is none.
    pub body: Vec<u8>,
}

impl Request {
    /// Every value of the header `name`, matched case-insensitively, in order.
    #[must_use]
    pub fn header(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// The first value of the header `name`, as Go's `Header.Get`: empty when absent.
    #[must_use]
    pub fn first_header(&self, name: &str) -> &str {
        self.header(name).first().copied().unwrap_or_default()
    }
}

/// A connection's stream: TCP or a unix socket.
pub trait Io: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// One client connection, as the [`Responder`] gets it.
pub struct Connection {
    stream: Box<dyn Io>,
    buffer: Vec<u8>,
    conn: usize,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("conn", &self.conn)
            .field("buffered", &self.buffer.len())
            .finish_non_exhaustive()
    }
}

/// What a fake does with each connection: reads its requests ([`Connection::read_request`]) and
/// answers them until it returns, which closes the connection.
pub type Responder =
    Arc<dyn Fn(Connection) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// A [`Responder`] from an async closure over the connection.
pub fn responder<F, Fut>(serve: F) -> Responder
where
    F: Fn(Connection) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |connection| Box::pin(serve(connection)))
}

/// The terminating chunk of a chunked body.
pub const LAST_CHUNK: &[u8] = b"0\r\n\r\n";

/// `data` as one chunk of a chunked body; nothing for empty data, which would end the body.
#[must_use]
pub fn chunk(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut out = format!("{:x}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    out
}

/// Go's `textproto.CanonicalMIMEHeaderKey` for a header name: each word's first letter upper
/// case, the rest lower case.
#[must_use]
pub fn canonical_key(name: &str) -> String {
    let mut upper = true;
    name.chars()
        .map(|character| {
            let mapped = if upper {
                character.to_ascii_uppercase()
            } else {
                character.to_ascii_lowercase()
            };
            upper = character == '-';
            mapped
        })
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A test's panic poisons it; the recording is still what it was.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Connection {
    /// The connection's number, from 1 in order of acceptance.
    #[must_use]
    pub fn conn(&self) -> usize {
        self.conn
    }

    async fn fill(&mut self) -> bool {
        let mut chunk = [0_u8; 4096];
        match self.stream.read(&mut chunk).await {
            Ok(0) | Err(_) => false,
            Ok(read) => {
                self.buffer.extend_from_slice(&chunk[..read]);
                true
            }
        }
    }

    /// The next request, its body read by its `Content-Length`, recorded; `None` once the client
    /// closed the connection (or sent something that is not a request line).
    pub async fn read_request(&mut self) -> Option<Request> {
        let end = loop {
            if let Some(end) = self
                .buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                break end;
            }
            if !self.fill().await {
                return None;
            }
        };
        let text = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
        self.buffer.drain(..end + 4);
        let mut lines = text.split("\r\n");
        let mut request_line = lines.next()?.split(' ');
        let method = request_line.next()?.to_owned();
        let target = request_line.next()?.to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
            .collect();
        let length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0);
        while self.buffer.len() < length {
            if !self.fill().await {
                return None;
            }
        }
        let body = self.buffer.drain(..length).collect();
        let request = Request {
            conn: self.conn,
            method,
            target,
            headers,
            body,
        };
        lock(&self.requests).push(request.clone());
        Some(request)
    }

    /// Writes `bytes`; false when the client is gone.
    pub async fn write_all(&mut self, bytes: &[u8]) -> bool {
        self.stream.write_all(bytes).await.is_ok() && self.stream.flush().await.is_ok()
    }

    /// An answer with a `Content-Length`: `HTTP/1.1 <status>` (`200 OK`), `headers` in order,
    /// then the length and, unless the request was a `HEAD`, the body. False when the client is
    /// gone.
    pub async fn respond(
        &mut self,
        status: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        method: &str,
    ) -> bool {
        let mut out = head(status, headers);
        let _ = write!(out, "Content-Length: {}\r\n\r\n", body.len());
        let mut bytes = out.into_bytes();
        if method != "HEAD" {
            bytes.extend_from_slice(body);
        }
        self.write_all(&bytes).await
    }

    /// The head of a chunked answer: `HTTP/1.1 <status>`, `headers`, `Transfer-Encoding:
    /// chunked`. The chunks follow with [`chunk`], and the body ends with [`LAST_CHUNK`], with
    /// [`Connection::shutdown`] (cut) or with [`Connection::hold_until_closed`].
    pub async fn start_chunked(&mut self, status: &str, headers: &[(&str, &str)]) -> bool {
        let mut out = head(status, headers);
        out.push_str("Transfer-Encoding: chunked\r\n\r\n");
        self.write_all(out.as_bytes()).await
    }

    /// Writes `data` as one chunk.
    pub async fn write_chunk(&mut self, data: &[u8]) -> bool {
        self.write_all(&chunk(data)).await
    }

    /// Writes the terminating chunk: a chunked body's clean end.
    pub async fn end_chunks(&mut self) -> bool {
        self.write_all(LAST_CHUNK).await
    }

    /// Closes the writing side, as a daemon or a proxy cuts a connection.
    pub async fn shutdown(&mut self) {
        let _ = self.stream.shutdown().await;
    }

    /// Reads and drops whatever the client sends until it goes away.
    pub async fn hold_until_closed(&mut self) {
        self.buffer.clear();
        while self.fill().await {
            self.buffer.clear();
        }
    }
}

fn head(status: &str, headers: &[(&str, &str)]) -> String {
    let mut out = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        let _ = write!(out, "{name}: {value}\r\n");
    }
    out
}

/// Where a fake listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    /// A TCP address.
    Tcp(SocketAddr),
    /// A unix socket path.
    Unix(PathBuf),
}

enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

impl Listener {
    async fn accept(&self) -> io::Result<Box<dyn Io>> {
        Ok(match self {
            Self::Tcp(listener) => Box::new(listener.accept().await?.0),
            Self::Unix(listener) => Box::new(listener.accept().await?.0),
        })
    }
}

/// A fake engine, serving until it is dropped.
#[derive(Debug)]
pub struct FakeEngine {
    listen: Listen,
    requests: Arc<Mutex<Vec<Request>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeEngine {
    /// Listens on the TCP address `bind` (`127.0.0.1:0`, `[::1]:0`), each connection served by
    /// `responder` on a task of its own.
    ///
    /// # Errors
    ///
    /// The bind's.
    pub async fn tcp(bind: &str, responder: Responder) -> io::Result<Self> {
        let listener = TcpListener::bind(bind).await?;
        let listen = Listen::Tcp(listener.local_addr()?);
        Ok(Self::serve(listen, responder, Listener::Tcp(listener)))
    }

    /// Listens on the unix socket `path`, each connection served by `responder` on a task of
    /// its own. The socket file stays; it goes with its directory (a [`super::TempDir`]).
    ///
    /// # Errors
    ///
    /// The bind's.
    pub fn unix(path: &Path, responder: Responder) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        let listen = Listen::Unix(path.to_owned());
        Ok(Self::serve(listen, responder, Listener::Unix(listener)))
    }

    fn serve(listen: Listen, responder: Responder, listener: Listener) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            let connections = AtomicUsize::new(0);
            while let Ok(stream) = listener.accept().await {
                let connection = Connection {
                    stream,
                    buffer: Vec::new(),
                    conn: connections.fetch_add(1, Ordering::SeqCst) + 1,
                    requests: Arc::clone(&recorded),
                };
                tokio::spawn(responder(connection));
            }
        });
        Self {
            listen,
            requests,
            server,
        }
    }

    /// Where it listens.
    #[must_use]
    pub fn listen(&self) -> &Listen {
        &self.listen
    }

    /// The `DOCKER_HOST` that names it: `tcp://<address>` or `unix://<path>`.
    #[must_use]
    pub fn host(&self) -> String {
        match &self.listen {
            Listen::Tcp(address) => format!("tcp://{address}"),
            Listen::Unix(path) => format!("unix://{}", path.display()),
        }
    }

    /// Every request read so far, in arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.requests).clone()
    }
}
