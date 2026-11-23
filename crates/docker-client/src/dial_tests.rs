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

//! A dial that fails reads as moby's client words it, on docker-client's own calls and on
//! bollard's through [`Error::at_host`].
//!
//! Ported from runhold/crates/runhold-docker/src/client.rs at `8e88406a`
//! (`dial_failure_tests`). The expected texts are the output of `TestPortGenDial` (moby client
//! v0.6.2) in the body of runhold commit `8e88406a`:
//!
//! ```text
//! tcp://127.0.0.1:1 | Cannot connect to the Docker daemon at tcp://127.0.0.1:1. Is the docker daemon running?
//! unix:///nonexistent/docker.sock | failed to connect to the docker API at unix:///nonexistent/docker.sock; check if the path is correct and if the daemon is running: dial unix /nonexistent/docker.sock: connect: no such file or directory
//! unix:///tmp/rh-dial/locked/x.sock | permission denied while trying to connect to the docker API at unix:///tmp/rh-dial/locked/x.sock
//! refused-unix | Cannot connect to the Docker daemon at unix:///tmp/rh-dial/plain.sock. Is the docker daemon running?
//! ```

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::errors::Error as BollardError;

use crate::{Config, Error, ErrorKind, bounded, connect};

const TIMEOUT: Duration = Duration::from_secs(5);

/// A short scratch directory under `/tmp`, so a socket path stays under the 108-byte limit.
fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dc-dial")
        .tempdir_in("/tmp")
        .unwrap()
}

/// A socket file nobody listens on: a dial to it is refused.
fn refused_socket(dir: &Path) -> PathBuf {
    let path = dir.join("plain.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    path
}

/// A directory closed to everyone but root, opened again when dropped (whatever the test's outcome,
/// a failed assertion included), so the scratch directory around it can be removed. Declare it
/// after that scratch directory, so it is dropped first.
struct Locked(PathBuf);

impl Locked {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

/// A closed directory in `dir`, or `None` when the tests run as root, for whom the case does not
/// exist.
fn closed_directory(dir: &Path) -> Option<Locked> {
    let locked = Locked(dir.join("locked"));
    std::fs::create_dir(locked.path()).unwrap();
    close(locked.path());
    std::fs::read_dir(locked.path()).is_err().then_some(locked)
}

fn close(dir: &Path) {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
}

/// Opens `dir` again.
fn open(dir: &Path) {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn unix_host(path: &Path) -> String {
    format!("unix://{}", path.display())
}

/// The error of a bollard call on a client connected from `config`, worded for its host.
async fn bollard_error(docker: &bollard::Docker, config: &Config) -> Error {
    bounded(TIMEOUT, docker.info())
        .await
        .unwrap_err()
        .at_host(config.daemon_host())
}

fn assert_dial(error: &Error, expected: &str) {
    assert_eq!(error.to_string(), expected);
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    let source = std::error::Error::source(error).expect("a dial failure has a source");
    assert!(source.is::<std::io::Error>(), "{source:?}");
}

#[test]
fn a_refused_dial_to_no_host_reads_as_moby_words_it() {
    let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
    let error = Error::from(BollardError::from(refused)).at_host("");
    assert_dial(
        &error,
        "Cannot connect to the Docker daemon. Is the docker daemon running on this host?",
    );
}

#[test]
fn a_missing_socket_from_bollard_reads_as_moby_words_it() {
    let error = Error::from(BollardError::SocketNotFoundError("/gone.sock".to_owned()))
        .at_host("unix:///gone.sock");
    assert_dial(
        &error,
        "failed to connect to the docker API at unix:///gone.sock; check if the path is correct \
         and if the daemon is running: dial unix /gone.sock: connect: no such file or directory",
    );
}

#[test]
fn errors_other_than_a_dial_come_back_unchanged() {
    let refusal = BollardError::DockerResponseServerError {
        status_code: 404,
        message: "No such container: x".to_owned(),
    };
    let error = Error::from(refusal).at_host("unix:///s");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert!(error.engine().is_some());
    // A dial failure moby has no text for keeps bollard's.
    let reset = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset by peer");
    let error = Error::from(BollardError::from(reset)).at_host("unix:///s");
    assert_eq!(error.to_string(), "reset by peer");
    assert!(error.engine().is_some());
    // A missing path is only moby's text for a unix host.
    let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
    let error = Error::from(BollardError::from(missing)).at_host("tcp://127.0.0.1:1");
    assert!(error.engine().is_some(), "{error}");
}

#[test]
fn an_io_error_from_bollard_is_the_source() {
    let reset = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset by peer");
    let error = Error::from(BollardError::from(reset));
    let source = std::error::Error::source(&error).unwrap();
    assert_eq!(
        source.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::ConnectionReset
    );
}

#[cfg(feature = "tcp")]
mod tcp {
    use super::*;

    const HOST: &str = "tcp://127.0.0.1:1";
    const TEXT: &str =
        "Cannot connect to the Docker daemon at tcp://127.0.0.1:1. Is the docker daemon running?";

    fn config() -> Config {
        Config::from_env(
            |name| (name == "DOCKER_HOST").then(|| HOST.to_owned()),
            crate::config::TEST_LIMITS,
        )
        .unwrap()
    }

    #[cfg(feature = "raw-get")]
    #[tokio::test]
    async fn a_refused_tcp_dial_reads_as_moby_words_it() {
        let config = config();
        let cancel = crate::CancellationToken::new();
        let error = crate::raw_get::get(&config, "/info", "", TIMEOUT, &cancel)
            .await
            .unwrap_err();
        assert_dial(&error, TEXT);
    }

    #[tokio::test]
    async fn a_refused_tcp_dial_from_bollard_reads_as_moby_words_it() {
        let config = config();
        let docker = connect(config.clone()).unwrap();
        assert_dial(&bollard_error(&docker, &config).await, TEXT);
    }
}

#[cfg(feature = "raw-get")]
mod own {
    use super::*;
    use crate::CancellationToken;
    use crate::test_util::{FakeEngine, Reply};

    async fn info_error(config: &Config) -> Error {
        crate::raw_get::get(config, "/info", "", TIMEOUT, &CancellationToken::new())
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn a_missing_socket_reads_as_moby_words_it() {
        let error = info_error(&Config::unix(
            "/nonexistent/docker.sock",
            crate::config::TEST_LIMITS,
        ))
        .await;
        assert_dial(
            &error,
            "failed to connect to the docker API at unix:///nonexistent/docker.sock; check if \
             the path is correct and if the daemon is running: dial unix \
             /nonexistent/docker.sock: connect: no such file or directory",
        );
    }

    #[tokio::test]
    async fn a_socket_nobody_listens_on_reads_as_moby_words_it() {
        let dir = scratch();
        let host = unix_host(&refused_socket(dir.path()));
        let error = info_error(&Config::unix(&host, crate::config::TEST_LIMITS)).await;
        assert_dial(
            &error,
            &format!(
                "Cannot connect to the Docker daemon at {host}. Is the docker daemon running?"
            ),
        );
    }

    #[tokio::test]
    async fn a_socket_behind_a_closed_directory_reads_as_moby_words_it() {
        let dir = scratch();
        let Some(locked) = closed_directory(dir.path()) else {
            return;
        };
        let host = unix_host(&locked.path().join("x.sock"));
        let error = info_error(&Config::unix(&host, crate::config::TEST_LIMITS)).await;
        assert_dial(
            &error,
            &format!("permission denied while trying to connect to the docker API at {host}"),
        );
    }

    /// A connection that breaks after the answer started is not a dial failure: it keeps its own
    /// text, and `at_host` leaves it alone.
    #[tokio::test]
    async fn a_broken_answer_keeps_its_own_text() {
        let engine = FakeEngine::start(&[(
            "/info",
            Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec()),
        )])
        .unwrap();
        let config = Config::unix(
            engine.socket().to_str().unwrap(),
            crate::config::TEST_LIMITS,
        );
        let error = info_error(&config).await;
        assert_eq!(error.kind(), ErrorKind::Unavailable, "{error}");
        assert!(error.engine().is_some(), "{error}");
        let text = error.to_string();
        assert!(!text.contains("docker daemon"), "{text}");
        assert!(!text.contains("docker API"), "{text}");
        assert_eq!(error.at_host(config.daemon_host()).to_string(), text);
    }
}

mod bollard_calls {
    use super::*;

    #[tokio::test]
    async fn a_missing_socket_reads_as_moby_words_it() {
        let dir = scratch();
        let path = dir.path().join("gone.sock");
        let engine_socket = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let config = Config::unix(path.to_str().unwrap(), crate::config::TEST_LIMITS);
        let docker = connect(config.clone()).unwrap();
        drop(engine_socket);
        std::fs::remove_file(&path).unwrap();
        let host = unix_host(&path);
        assert_dial(
            &bollard_error(&docker, &config).await,
            &format!(
                "failed to connect to the docker API at {host}; check if the path is correct and \
                 if the daemon is running: dial unix {}: connect: no such file or directory",
                path.display()
            ),
        );
    }

    #[tokio::test]
    async fn a_socket_nobody_listens_on_reads_as_moby_words_it() {
        let dir = scratch();
        let path = refused_socket(dir.path());
        let config = Config::unix(path.to_str().unwrap(), crate::config::TEST_LIMITS);
        let docker = connect(config.clone()).unwrap();
        let host = unix_host(&path);
        assert_dial(
            &bollard_error(&docker, &config).await,
            &format!(
                "Cannot connect to the Docker daemon at {host}. Is the docker daemon running?"
            ),
        );
    }

    #[tokio::test]
    async fn a_socket_behind_a_closed_directory_reads_as_moby_words_it() {
        let dir = scratch();
        let Some(locked) = closed_directory(dir.path()) else {
            return;
        };
        open(locked.path());
        let path = refused_socket(locked.path());
        let config = Config::unix(path.to_str().unwrap(), crate::config::TEST_LIMITS);
        let docker = connect(config.clone()).unwrap();
        close(locked.path());
        let error = bollard_error(&docker, &config).await;
        let host = unix_host(&path);
        assert_dial(
            &error,
            &format!("permission denied while trying to connect to the docker API at {host}"),
        );
    }

    /// bollard refuses a socket path it cannot see at connect; moby would dial it at the first
    /// call, so the error is the dial's.
    #[tokio::test]
    async fn connect_words_a_socket_behind_a_closed_directory_as_moby_does() {
        let dir = scratch();
        let Some(locked) = closed_directory(dir.path()) else {
            return;
        };
        let host = unix_host(&locked.path().join("x.sock"));
        let error = connect(Config::unix(&host, crate::config::TEST_LIMITS)).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("permission denied while trying to connect to the docker API at {host}")
        );
        assert_dial(
            &Error::connect(error),
            &format!("permission denied while trying to connect to the docker API at {host}"),
        );
    }
}

/// A connection that ends in an I/O error of `kind` where its peer closed it, as a keepalive that
/// gave up ends a read.
struct FailingEnd {
    inner: tokio::io::DuplexStream,
    kind: std::io::ErrorKind,
}

impl tokio::io::AsyncRead for FailingEnd {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let kind = self.kind;
        let poll = std::pin::Pin::new(&mut self.inner).poll_read(context, buf);
        if matches!(poll, std::task::Poll::Ready(Ok(()))) && buf.filled().len() == before {
            return std::task::Poll::Ready(Err(std::io::Error::from(kind)));
        }
        poll
    }
}

impl tokio::io::AsyncWrite for FailingEnd {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(context, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

/// hyper's error for a body that broke off after the answer started, with an I/O error of `kind`
/// in its cause chain.
async fn broken_body(kind: std::io::ErrorKind) -> hyper::Error {
    use http_body_util::BodyExt as _;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let (client, mut server) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let mut request = [0_u8; 1024];
        let _ = server.read(&mut request).await;
        server
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort")
            .await
            .unwrap();
    });
    let io = hyper_util::rt::TokioIo::new(FailingEnd {
        inner: client,
        kind,
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(connection);
    let request = http::Request::builder()
        .uri("/info")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    response.into_body().collect().await.unwrap_err()
}

/// A connection that broke after the answer started is not a dial failure, even with an I/O error
/// moby would word as one (a keepalive that timed out, a refusal) under it: `at_host` leaves it as
/// it is.
#[tokio::test]
async fn a_broken_bollard_stream_is_not_a_dial_failure() {
    for kind in [
        std::io::ErrorKind::TimedOut,
        std::io::ErrorKind::ConnectionRefused,
        std::io::ErrorKind::PermissionDenied,
    ] {
        let broken = broken_body(kind).await;
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&broken);
        let mut carried = None;
        while let Some(current) = cause {
            carried = carried.or_else(|| {
                current
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind)
            });
            cause = current.source();
        }
        assert_eq!(carried, Some(kind), "the case carries its I/O error");
        let error = Error::from(BollardError::from(broken));
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        let text = error.to_string();
        let worded = error.at_host("tcp://127.0.0.1:1");
        assert_eq!(worded.to_string(), text, "{kind:?}");
        assert!(worded.engine().is_some(), "{kind:?}");
    }
}
