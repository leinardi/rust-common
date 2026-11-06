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

use std::collections::HashMap;
use std::time::Duration;

use bollard::Docker;
use bollard::errors::Error as BollardError;
use futures_util::stream;
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::test_util::{FakeEngine, Reply};
use crate::*;

fn client(engine: &FakeEngine) -> Docker {
    connect(Config::unix(engine.socket().to_str().unwrap())).unwrap()
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

async fn inspect(docker: &Docker, id: &str) -> Result<(), Error> {
    bounded(Duration::from_secs(5), docker.inspect_container(id, None))
        .await
        .map(|_| ())
}

#[tokio::test]
async fn a_404_is_not_found_and_a_409_a_conflict() {
    let engine = FakeEngine::start(&[
        ("/containers/gone/json", Reply::not_found()),
        ("/containers/taken/json", Reply::conflict()),
        ("/containers/broken/json", Reply::error(500, "boom")),
    ])
    .unwrap();
    let docker = client(&engine);
    let gone = inspect(&docker, "gone").await.unwrap_err();
    assert_eq!(gone.kind(), ErrorKind::NotFound);
    assert!(gone.to_string().contains("No such object"), "{gone}");
    let taken = inspect(&docker, "taken").await.unwrap_err();
    assert_eq!(taken.kind(), ErrorKind::Conflict);
    let broken = inspect(&docker, "broken").await.unwrap_err();
    assert_eq!(broken.kind(), ErrorKind::Other);
    // A route the engine does not know is a 404 as well.
    let unknown = inspect(&docker, "other").await.unwrap_err();
    assert_eq!(unknown.kind(), ErrorKind::NotFound);
    assert!(matches!(
        unknown.engine(),
        Some(BollardError::DockerResponseServerError {
            status_code: 404,
            ..
        })
    ));
}

#[tokio::test]
async fn a_refused_connection_is_unavailable() {
    let engine = FakeEngine::start(&[]).unwrap();
    let socket = engine.socket().with_file_name("refused.sock");
    // A socket file nobody listens on: the connection is refused.
    drop(UnixListener::bind(&socket).unwrap());
    let docker = connect(Config::unix(socket.to_str().unwrap())).unwrap();
    let error = inspect(&docker, "x").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unavailable, "{error}");
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test]
async fn a_missing_socket_is_refused_at_connect() {
    let error = connect(Config::unix("/nonexistent/docker.sock")).unwrap_err();
    assert!(matches!(
        error,
        ConnectError::Client(BollardError::SocketNotFoundError(_))
    ));
}

/// An engine that sends the response headers at once and then stalls in the body: the call fails
/// with a timeout after the bound, not whenever the body ends. bollard's own timeout stops at the
/// headers.
#[tokio::test]
async fn a_stalled_body_times_out() {
    let engine = FakeEngine::start(&[("/containers/json", Reply::Stalled)]).unwrap();
    let docker = client(&engine);
    let timeout = Duration::from_millis(300);
    let started = Instant::now();
    let error = bounded(timeout, docker.list_containers(None))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(error.engine().is_none());
    assert!(elapsed >= timeout, "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

/// A stream that never yields and tells when it is dropped, which is when the pump's task ended.
fn idle_stream() -> (
    impl futures_util::Stream<Item = Result<u8, BollardError>> + Send,
    oneshot::Receiver<()>,
) {
    struct Guard(Option<oneshot::Sender<()>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let (sender, dropped) = oneshot::channel();
    let items = stream::unfold(Guard(Some(sender)), |guard| async move {
        std::future::pending::<()>().await;
        Some((Ok(0), guard))
    });
    (items, dropped)
}

async fn ended(dropped: oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(5), dropped)
        .await
        .expect("the pump's task did not end")
        .unwrap();
}

#[tokio::test]
async fn the_pump_stops_when_the_receiver_is_dropped() {
    let (items, dropped) = idle_stream();
    let receiver = pump_stream(items, 4, CancellationToken::new());
    drop(receiver);
    ended(dropped).await;
}

#[tokio::test]
async fn the_pump_stops_when_cancelled() {
    let (items, dropped) = idle_stream();
    let cancel = CancellationToken::new();
    let mut receiver = pump_stream(items, 4, cancel.clone());
    cancel.cancel();
    ended(dropped).await;
    assert!(receiver.recv().await.is_none());
}

#[tokio::test]
async fn the_pump_stops_when_cancelled_with_a_full_unread_channel() {
    let items = stream::repeat_with(|| Ok::<_, BollardError>(1_u8));
    let cancel = CancellationToken::new();
    let mut receiver = pump_stream(items, 1, cancel.clone());
    // Let the task fill the channel and block on the send, then cancel.
    tokio::task::yield_now().await;
    cancel.cancel();
    while receiver.recv().await.is_some() {}
}

#[tokio::test]
async fn the_pump_sorts_errors_and_goes_on() {
    let items = stream::iter([Ok(1_u8), Err(BollardError::RequestTimeoutError), Ok(2)]);
    let mut receiver = pump_stream(items, 0, CancellationToken::new());
    assert_eq!(receiver.recv().await.unwrap().unwrap(), 1);
    assert_eq!(
        receiver.recv().await.unwrap().unwrap_err().kind(),
        ErrorKind::Timeout
    );
    assert_eq!(receiver.recv().await.unwrap().unwrap(), 2);
    assert!(receiver.recv().await.is_none());
}

#[tokio::test]
async fn an_event_stream_that_closes_ends_the_pump() {
    let engine = FakeEngine::start(&[(
        "/events",
        Reply::events(&[
            r#"{"Type":"container","Action":"start"}"#,
            r#"{"Type":"container","Action":"die"}"#,
        ]),
    )])
    .unwrap();
    let docker = client(&engine);
    let mut receiver = pump_stream(docker.events(None), 4, CancellationToken::new());
    let mut actions = Vec::new();
    while let Some(event) = receiver.recv().await {
        actions.push(event.unwrap().action.unwrap());
    }
    assert_eq!(actions, ["start", "die"]);
}

#[tokio::test]
async fn event_parameters_reach_the_engine() {
    let engine = FakeEngine::start(&[("/events", Reply::events(&[]))]).unwrap();
    let docker = client(&engine);
    let filters = HashMap::from([("type", vec!["container"])]);
    let options = bollard::query_parameters::EventsOptionsBuilder::new()
        .since("1700000000")
        .filters(&filters)
        .build();
    let mut receiver = pump_stream(docker.events(Some(options)), 4, CancellationToken::new());
    assert!(receiver.recv().await.is_none());
    let requests = engine.requests();
    let query = requests[0].query();
    assert!(query.contains("since=1700000000"), "{query}");
    assert!(
        query.contains("filters=%7B%22type%22%3A%5B%22container%22%5D%7D"),
        "{query}"
    );
}

#[tokio::test]
async fn a_304_is_a_success_not_an_error() {
    // bollard counts 304 Not Modified as a success: a start of a running container, a stop of a
    // stopped one, return Ok, so there is no error kind for it.
    let engine = FakeEngine::start(&[
        ("/containers/c/start", Reply::status(304)),
        ("/containers/c/stop", Reply::status(304)),
    ])
    .unwrap();
    let docker = client(&engine);
    bounded(Duration::from_secs(5), docker.start_container("c", None))
        .await
        .unwrap();
    let options = bollard::query_parameters::StopContainerOptionsBuilder::new()
        .t(7)
        .build();
    bounded(
        Duration::from_secs(5),
        docker.stop_container("c", Some(options)),
    )
    .await
    .unwrap();
    let requests = engine.requests();
    assert_eq!(requests[1].query(), "t=7");
}

#[tokio::test]
async fn the_user_agent_is_sent_and_a_bad_one_refused() {
    let engine = FakeEngine::start(&[("/_ping", Reply::ok("OK"))]).unwrap();
    let socket = engine.socket().to_str().unwrap();
    let docker = connect(Config::unix(socket).user_agent("tool/1.2")).unwrap();
    docker.ping().await.unwrap();
    let requests = engine.requests();
    assert_eq!(requests[0].header("user-agent"), Some("tool/1.2"));
    let bad = connect(Config::unix(socket).user_agent("a\nb")).unwrap_err();
    assert!(matches!(bad, ConnectError::Invalid(_)), "{bad}");
}

#[tokio::test]
async fn a_fixed_version_is_kept_and_otherwise_negotiated() {
    let engine = FakeEngine::start(&[("/version", Reply::ok(r#"{"ApiVersion":"1.41"}"#))]).unwrap();
    let socket = engine.socket().to_str().unwrap();

    let fixed = Config::unix(socket).api_version("1.44").unwrap();
    let docker = negotiate(connect(fixed.clone()).unwrap(), &fixed)
        .await
        .unwrap();
    assert_eq!(docker.client_version().to_string(), "1.44");
    assert!(engine.requests().is_empty());

    let open = Config::unix(socket);
    let docker = negotiate(connect(open.clone()).unwrap(), &open)
        .await
        .unwrap();
    assert_eq!(docker.client_version().to_string(), "1.41");
    assert_eq!(engine.requests().len(), 1);
}

#[test]
fn from_env_defaults_to_the_standard_socket_and_ignores_empty_variables() {
    let config = Config::from_env(env(&[("DOCKER_HOST", ""), ("DOCKER_API_VERSION", "")])).unwrap();
    assert_eq!(
        config.endpoint,
        crate::config::Endpoint::Unix("/var/run/docker.sock".to_owned())
    );
    assert!(config.api_version.is_none());
}

#[test]
fn from_env_reads_a_unix_host_and_the_api_version() {
    let config = Config::from_env(env(&[
        ("DOCKER_HOST", "unix:///run/user/1000/docker.sock"),
        ("DOCKER_API_VERSION", "1.43"),
        // A unix host needs no TLS.
        ("DOCKER_TLS_VERIFY", "1"),
    ]))
    .unwrap();
    assert_eq!(
        config.endpoint,
        crate::config::Endpoint::Unix("/run/user/1000/docker.sock".to_owned())
    );
    let version = config.api_version.unwrap();
    assert_eq!((version.major_version, version.minor_version), (1, 43));
}

#[test]
fn from_env_refuses_what_is_not_an_address_or_a_version() {
    for host in ["unix://", "tcp://", "ftp://x", "/var/run/docker.sock"] {
        let error = Config::from_env(env(&[("DOCKER_HOST", host)])).unwrap_err();
        assert!(matches!(error, ConnectError::Invalid(_)), "{host}: {error}");
    }
    for version in ["1", "1.", ".43", "1.x", "1.2.3", "v1.43", "-1.43", "+1.43"] {
        let error = Config::from_env(env(&[("DOCKER_API_VERSION", version)])).unwrap_err();
        assert!(
            matches!(error, ConnectError::Invalid(_)),
            "{version}: {error}"
        );
    }
    assert!(Config::unix("/s").api_version("1.43").is_ok());
    assert!(Config::unix("/s").api_version("x").is_err());
}

#[cfg(not(feature = "tcp"))]
#[test]
fn from_env_needs_the_tcp_feature_for_a_tcp_host() {
    for host in ["tcp://127.0.0.1:2375", "http://h:1", "https://h:2376"] {
        let error = Config::from_env(env(&[("DOCKER_HOST", host)])).unwrap_err();
        assert!(
            matches!(error, ConnectError::Unsupported(_)),
            "{host}: {error}"
        );
    }
}

#[test]
fn from_env_refuses_a_plain_http_host_with_tls_verify() {
    for verify in ["1", "0"] {
        let error = Config::from_env(env(&[
            ("DOCKER_HOST", "http://h:2375"),
            ("DOCKER_TLS_VERIFY", verify),
        ]))
        .unwrap_err();
        let ConnectError::Invalid(text) = &error else {
            panic!("{error:?}");
        };
        assert!(
            text.contains("http://") && text.contains("DOCKER_TLS_VERIFY"),
            "{text}"
        );
    }
}

#[cfg(feature = "tcp")]
#[test]
fn from_env_reads_a_tcp_host() {
    for host in ["tcp://127.0.0.1:2375", "http://127.0.0.1:2375"] {
        let config = Config::from_env(env(&[("DOCKER_HOST", host)])).unwrap();
        assert_eq!(
            config.endpoint,
            crate::config::Endpoint::Tcp("tcp://127.0.0.1:2375".to_owned())
        );
        connect(config).unwrap();
    }
}

#[cfg(all(feature = "tcp", not(feature = "tls")))]
#[test]
fn from_env_needs_the_tls_feature_for_tls() {
    for pairs in [
        &[("DOCKER_HOST", "https://h:2376")][..],
        &[("DOCKER_HOST", "tcp://h:2376"), ("DOCKER_TLS_VERIFY", "1")][..],
    ] {
        let error = Config::from_env(env(pairs)).unwrap_err();
        assert!(matches!(error, ConnectError::Unsupported(_)), "{error}");
    }
}

#[cfg(feature = "tls")]
#[test]
fn from_env_reads_tls_from_the_cert_path() {
    let expected = crate::config::Endpoint::Tls {
        addr: "tcp://h:2376".to_owned(),
        certs: "/certs".into(),
    };
    for pairs in [
        &[
            ("DOCKER_HOST", "tcp://h:2376"),
            ("DOCKER_TLS_VERIFY", "1"),
            ("DOCKER_CERT_PATH", "/certs"),
        ][..],
        &[
            ("DOCKER_HOST", "https://h:2376"),
            ("DOCKER_CERT_PATH", "/certs"),
        ][..],
    ] {
        assert_eq!(Config::from_env(env(pairs)).unwrap().endpoint, expected);
    }
    // An unset or empty DOCKER_CERT_PATH falls back to $HOME/.docker, as moby does; with no HOME
    // either, there is nowhere to read from. Any non-empty DOCKER_TLS_VERIFY counts, `0` too.
    let home = crate::config::Endpoint::Tls {
        addr: "tcp://h:2376".to_owned(),
        certs: "/home/u/.docker".into(),
    };
    for pairs in [
        &[
            ("DOCKER_HOST", "tcp://h:2376"),
            ("DOCKER_TLS_VERIFY", "0"),
            ("HOME", "/home/u"),
        ][..],
        &[
            ("DOCKER_HOST", "https://h:2376"),
            ("DOCKER_CERT_PATH", ""),
            ("HOME", "/home/u"),
        ][..],
    ] {
        assert_eq!(Config::from_env(env(pairs)).unwrap().endpoint, home);
    }
    for pairs in [
        &[("DOCKER_HOST", "tcp://h:2376"), ("DOCKER_TLS_VERIFY", "1")][..],
        &[("DOCKER_HOST", "https://h:2376"), ("DOCKER_CERT_PATH", "")][..],
    ] {
        let error = Config::from_env(env(pairs)).unwrap_err();
        assert!(matches!(error, ConnectError::Invalid(_)), "{error}");
    }
    // Without DOCKER_TLS_VERIFY a tcp host stays plain, certificates or not.
    let plain = Config::from_env(env(&[
        ("DOCKER_HOST", "tcp://h:2375"),
        ("DOCKER_CERT_PATH", "/certs"),
    ]))
    .unwrap();
    assert_eq!(
        plain.endpoint,
        crate::config::Endpoint::Tcp("tcp://h:2375".to_owned())
    );
}

#[cfg(feature = "attach")]
mod attach_tests {
    use super::*;
    use crate::attach::{Output, WaitCondition, attach, wait};

    const SECS: Duration = Duration::from_secs(5);

    fn pair(engine: &FakeEngine) -> (Docker, Config) {
        let config = Config::unix(engine.socket().to_str().unwrap());
        (connect(config.clone()).unwrap(), config)
    }

    fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![kind, 0, 0, 0];
        bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[tokio::test]
    async fn attach_splits_stdout_from_stderr_and_closes_with_the_stream() {
        let mut stream = frame(1, b"out");
        stream.extend(frame(2, b"err"));
        let engine =
            FakeEngine::start(&[("/containers/c/attach", Reply::upgrade(&stream))]).unwrap();
        let docker = client(&engine);
        let mut receiver = attach(
            &docker,
            "c",
            Duration::from_secs(5),
            4,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            receiver.recv().await.unwrap().unwrap(),
            Output::Stdout(b"out".to_vec())
        );
        assert_eq!(
            receiver.recv().await.unwrap().unwrap(),
            Output::Stderr(b"err".to_vec())
        );
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn attach_to_a_missing_container_is_not_found() {
        let engine = FakeEngine::start(&[]).unwrap();
        let docker = client(&engine);
        let error = attach(
            &docker,
            "c",
            Duration::from_secs(5),
            4,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn wait_answers_an_exit_code_zero_or_not() {
        let engine = FakeEngine::start(&[
            ("/containers/ok/wait", Reply::ok(r#"{"StatusCode":0}"#)),
            ("/containers/bad/wait", Reply::ok(r#"{"StatusCode":3}"#)),
            (
                "/containers/err/wait",
                Reply::ok(r#"{"StatusCode":137,"Error":{"Message":"oom"}}"#),
            ),
        ])
        .unwrap();
        let (docker, config) = pair(&engine);
        for (id, code) in [("ok", 0), ("bad", 3), ("err", 137)] {
            let answer = wait(
                &docker,
                &config,
                id,
                WaitCondition::NotRunning,
                SECS,
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(answer.await.unwrap().unwrap(), code, "{id}");
        }
    }

    #[tokio::test]
    async fn wait_sends_the_condition_asked_for() {
        let engine =
            FakeEngine::start(&[("/containers/c/wait", Reply::ok(r#"{"StatusCode":0}"#))]).unwrap();
        let (docker, config) = pair(&engine);
        for (condition, query) in [
            (WaitCondition::NotRunning, "condition=not-running"),
            (WaitCondition::NextExit, "condition=next-exit"),
            (WaitCondition::Removed, "condition=removed"),
        ] {
            wait(
                &docker,
                &config,
                "c",
                condition,
                SECS,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .await
            .unwrap()
            .unwrap();
            let requests = engine.requests();
            assert_eq!(requests.last().unwrap().query(), query);
            assert_eq!(requests.last().unwrap().method, "POST");
        }
    }

    /// A wait that reaches the engine after the container is gone is a 404: the call itself fails
    /// as `NotFound`, which a `Removed` caller reads as "already removed".
    #[tokio::test]
    async fn wait_removed_on_a_missing_container_delivers_not_found() {
        let engine = FakeEngine::start(&[("/containers/gone/wait", Reply::not_found())]).unwrap();
        let (docker, config) = pair(&engine);
        let error = wait(
            &docker,
            &config,
            "gone",
            WaitCondition::Removed,
            SECS,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(engine.requests()[0].query(), "condition=removed");
    }

    /// Cancelling ends the task that reads the exit code.
    #[tokio::test]
    async fn wait_ends_when_cancelled() {
        let engine = FakeEngine::start(&[("/containers/c/wait", Reply::Stalled)]).unwrap();
        let (docker, config) = pair(&engine);
        let cancel = CancellationToken::new();
        let receiver = wait(
            &docker,
            &config,
            "c",
            WaitCondition::NextExit,
            SECS,
            cancel.clone(),
        )
        .await
        .unwrap();
        cancel.cancel();
        assert!(receiver.await.is_err());
    }

    /// The call returns once the engine answered the headers, and not before: the engine has the
    /// request by then, so a start made next cannot overtake it. The exit code, sent later, still
    /// arrives; the User-Agent and the version prefix are the client's.
    #[tokio::test]
    async fn wait_returns_after_the_headers_and_before_the_exit() {
        let engine = FakeEngine::start(&[
            (
                "/containers/slow/wait",
                Reply::Delayed {
                    delay: Duration::from_millis(300),
                    reply: Box::new(Reply::HeadersThenBody {
                        body_delay: Duration::from_millis(300),
                        body: r#"{"StatusCode":7}"#.to_owned(),
                    }),
                },
            ),
            ("/containers/slow/start", Reply::status(204)),
        ])
        .unwrap();
        let config = Config::unix(engine.socket().to_str().unwrap())
            .api_version("1.41")
            .unwrap()
            .user_agent("tool/1.2");
        let docker = connect(config.clone()).unwrap();
        let begun = Instant::now();
        let receiver = wait(
            &docker,
            &config,
            "slow",
            WaitCondition::NextExit,
            SECS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let headers_after = begun.elapsed();
        assert!(
            headers_after >= Duration::from_millis(250),
            "{headers_after:?}"
        );
        assert!(
            headers_after < Duration::from_millis(550),
            "{headers_after:?}"
        );
        docker
            .start_container(
                "slow",
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .unwrap();
        assert_eq!(receiver.await.unwrap().unwrap(), 7);
        let requests = engine.requests();
        let order: Vec<&str> = requests
            .iter()
            .map(crate::test_util::Request::path)
            .collect();
        assert_eq!(order, ["/containers/slow/wait", "/containers/slow/start"]);
        assert_eq!(
            requests[0].raw_path(),
            "/v1.41/containers/slow/wait?condition=next-exit"
        );
        assert_eq!(requests[0].header("user-agent"), Some("tool/1.2"));
    }

    /// bollard's calls and the header-awaiting ones carry the same prefix after `negotiate`,
    /// whether the engine lowered the version or is newer than bollard's default.
    #[tokio::test]
    async fn streaming_calls_carry_the_prefix_bollard_calls_do_after_negotiating() {
        for (api, prefix) in [("99.0", ""), ("1.41", "/v1.41")] {
            let engine = FakeEngine::start(&[
                (
                    "/version",
                    Reply::ok(&format!(r#"{{"ApiVersion":"{api}"}}"#)),
                ),
                ("/containers/json", Reply::ok("[]")),
                ("/containers/c/wait", Reply::ok(r#"{"StatusCode":0}"#)),
                ("/events", Reply::events(&[])),
            ])
            .unwrap();
            let (docker, config) = pair(&engine);
            let docker = negotiate(docker, &config).await.unwrap();
            docker
                .list_containers(None::<bollard::query_parameters::ListContainersOptions>)
                .await
                .unwrap();
            wait(
                &docker,
                &config,
                "c",
                WaitCondition::NextExit,
                SECS,
                CancellationToken::new(),
            )
            .await
            .unwrap();
            let _events = events_established(&docker, &config, None, SECS)
                .await
                .unwrap();
            let paths: Vec<String> = engine
                .requests()
                .iter()
                .skip(1)
                .map(|request| request.raw_path().split('?').next().unwrap().to_owned())
                .collect();
            assert_eq!(
                paths,
                [
                    format!("{prefix}/containers/json"),
                    format!("{prefix}/containers/c/wait"),
                    format!("{prefix}/events"),
                ],
                "{api}"
            );
        }
    }

    /// `Config::unix` takes a `unix://` path as bollard does, and the streaming calls connect to it.
    #[tokio::test]
    async fn a_unix_scheme_path_works_for_streaming_calls() {
        let engine =
            FakeEngine::start(&[("/containers/c/wait", Reply::ok(r#"{"StatusCode":4}"#))]).unwrap();
        let config = Config::unix(format!("unix://{}", engine.socket().to_str().unwrap()));
        let docker = connect(config.clone()).unwrap();
        let receiver = wait(
            &docker,
            &config,
            "c",
            WaitCondition::NextExit,
            SECS,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(receiver.await.unwrap().unwrap(), 4);
    }

    #[tokio::test]
    async fn wait_times_out_when_the_headers_never_come() {
        let engine = FakeEngine::start(&[("/containers/c/wait", Reply::Silent)]).unwrap();
        let (docker, config) = pair(&engine);
        let begun = Instant::now();
        let error = wait(
            &docker,
            &config,
            "c",
            WaitCondition::NextExit,
            Duration::from_millis(200),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(begun.elapsed() < Duration::from_secs(2));
    }
}

#[tokio::test]
async fn requests_carry_the_fixed_api_version() {
    let engine = FakeEngine::start(&[("/containers/json", Reply::ok("[]"))]).unwrap();
    let socket = engine.socket().to_str().unwrap();
    let config = Config::from_env(env(&[
        ("DOCKER_HOST", &format!("unix://{socket}")),
        ("DOCKER_API_VERSION", "1.43"),
    ]))
    .unwrap();
    let docker = negotiate(connect(config.clone()).unwrap(), &config)
        .await
        .unwrap();
    docker
        .list_containers(None::<bollard::query_parameters::ListContainersOptions>)
        .await
        .unwrap();
    let requests = engine.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].raw_path(), "/v1.43/containers/json");
    assert_eq!(requests[0].path(), "/containers/json");
}

#[tokio::test]
async fn requests_carry_the_negotiated_version_and_negotiation_itself_is_unversioned() {
    let engine = FakeEngine::start(&[
        ("/version", Reply::ok(r#"{"ApiVersion":"1.41"}"#)),
        ("/containers/json", Reply::ok("[]")),
    ])
    .unwrap();
    let socket = engine.socket().to_str().unwrap();
    let config = Config::unix(socket).user_agent("tool/1.2");
    let docker = connect(config.clone()).unwrap();

    // Before negotiation: unversioned, as bollard has always sent it.
    docker
        .list_containers(None::<bollard::query_parameters::ListContainersOptions>)
        .await
        .unwrap();
    let docker = negotiate(docker, &config).await.unwrap();
    docker
        .list_containers(None::<bollard::query_parameters::ListContainersOptions>)
        .await
        .unwrap();

    let targets: Vec<String> = engine
        .requests()
        .iter()
        .map(|request| request.raw_path().to_owned())
        .collect();
    assert_eq!(
        targets,
        ["/containers/json", "/version", "/v1.41/containers/json"]
    );
    // The other modifier duty survives.
    assert_eq!(engine.requests()[2].header("user-agent"), Some("tool/1.2"));
}

#[test]
fn a_version_prefix_is_added_once_and_the_query_kept() {
    use crate::config::versioned;
    let version = bollard::ClientVersion {
        major_version: 1,
        minor_version: 44,
    };
    let uri = |text: &str| text.parse::<http::Uri>().unwrap();
    assert_eq!(
        versioned(&uri("http://h/containers/json?all=1"), version),
        Some(uri("http://h/v1.44/containers/json?all=1"))
    );
    assert_eq!(versioned(&uri("http://h/v1.40/version"), version), None);
    // `/volumes` starts with a `v` but is not a version.
    assert_eq!(
        versioned(&uri("http://h/volumes"), version),
        Some(uri("http://h/v1.44/volumes"))
    );
}

#[tokio::test]
async fn dropping_the_engine_closes_the_connections_it_is_stalling() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::UnixStream;

    let engine = FakeEngine::start(&[("/containers/json", Reply::Stalled)]).unwrap();
    let mut stream = UnixStream::connect(engine.socket()).await.unwrap();
    stream
        .write_all(b"GET /containers/json HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut seen = Vec::new();
    while !seen.ends_with(b"[") {
        let mut chunk = [0_u8; 256];
        let read = stream.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "closed before the stall");
        seen.extend_from_slice(&chunk[..read]);
    }
    drop(engine);
    let mut rest = [0_u8; 16];
    let closed = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut rest))
        .await
        .expect("the stalled connection stays open after the engine is dropped");
    assert!(matches!(closed, Ok(0) | Err(_)), "{closed:?}");
}

#[tokio::test]
async fn clients_from_clones_of_one_config_keep_their_own_version() {
    let engine = FakeEngine::start(&[
        ("/version", Reply::ok(r#"{"ApiVersion":"1.41"}"#)),
        ("/containers/json", Reply::ok("[]")),
    ])
    .unwrap();
    let socket = engine.socket().to_str().unwrap();
    let base = Config::unix(socket);
    let old_config = base.clone().api_version("1.41").unwrap();
    let new_config = base.clone().api_version("1.44").unwrap();
    let old = connect(old_config).unwrap();
    let new = connect(new_config).unwrap();
    // A client negotiated from the shared base must not move the pinned ones.
    let open = negotiate(connect(base.clone()).unwrap(), &base)
        .await
        .unwrap();
    for docker in [&old, &new, &open] {
        docker
            .list_containers(None::<bollard::query_parameters::ListContainersOptions>)
            .await
            .unwrap();
    }
    let targets: Vec<String> = engine
        .requests()
        .iter()
        .map(|request| request.raw_path().to_owned())
        .collect();
    assert_eq!(
        targets,
        [
            "/version",
            "/v1.41/containers/json",
            "/v1.44/containers/json",
            "/v1.41/containers/json"
        ]
    );
}

#[tokio::test]
async fn a_clone_of_the_config_shares_no_negotiated_version() {
    let engine = FakeEngine::start(&[
        ("/version", Reply::ok(r#"{"ApiVersion":"1.41"}"#)),
        ("/containers/json", Reply::ok("[]")),
    ])
    .unwrap();
    let config = Config::unix(engine.socket().to_str().unwrap());
    let first = connect(config.clone()).unwrap();
    let second = connect(config.clone()).unwrap();
    let _negotiated = negotiate(first, &config).await.unwrap();
    second
        .list_containers(None::<bollard::query_parameters::ListContainersOptions>)
        .await
        .unwrap();
    let requests = engine.requests();
    assert_eq!(requests[1].raw_path(), "/containers/json");
}

const EVENT_A: &str = r#"{"Type":"container","Action":"start","Actor":{"ID":"a","Attributes":{}}}"#;

fn pair(engine: &FakeEngine) -> (Docker, Config) {
    let config = Config::unix(engine.socket().to_str().unwrap());
    (connect(config.clone()).unwrap(), config)
}

#[tokio::test]
async fn events_established_streams_lines_and_survives_an_undecodable_one() {
    use futures_util::StreamExt as _;

    let engine =
        FakeEngine::start(&[("/events", Reply::events(&[EVENT_A, "not json", EVENT_A]))]).unwrap();
    let (docker, config) = pair(&engine);
    let mut events = events_established(&docker, &config, None, Duration::from_secs(5))
        .await
        .unwrap();
    let first = events.next().await.unwrap().unwrap();
    assert_eq!(first.action.as_deref(), Some("start"));
    assert!(events.next().await.unwrap().is_err());
    assert!(events.next().await.unwrap().is_ok());
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn events_established_sends_the_options_the_agent_and_the_version() {
    use bollard::query_parameters::EventsOptionsBuilder;

    let engine = FakeEngine::start(&[("/events", Reply::events(&[]))]).unwrap();
    let config = Config::unix(engine.socket().to_str().unwrap())
        .api_version("1.43")
        .unwrap()
        .user_agent("tool/1.2");
    let docker = connect(config.clone()).unwrap();
    let options = EventsOptionsBuilder::new()
        .since("100")
        .filters(&HashMap::from([("type", vec!["container"])]))
        .build();
    let _events = events_established(&docker, &config, Some(options), Duration::from_secs(5))
        .await
        .unwrap();
    let requests = engine.requests();
    assert!(requests[0].raw_path().starts_with("/v1.43/events?"));
    assert!(
        requests[0].query().contains("since=100"),
        "{}",
        requests[0].query()
    );
    assert!(
        requests[0].query().contains("filters="),
        "{}",
        requests[0].query()
    );
    assert_eq!(requests[0].header("user-agent"), Some("tool/1.2"));
}

#[tokio::test]
async fn events_established_times_out_on_stalled_headers_and_fails_fast_on_a_refusal() {
    let engine = FakeEngine::start(&[("/events", Reply::Silent)]).unwrap();
    let (docker, config) = pair(&engine);
    let begun = Instant::now();
    let Err(error) = events_established(&docker, &config, None, Duration::from_millis(200)).await
    else {
        panic!("a subscription nobody answered was established");
    };
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(begun.elapsed() < Duration::from_secs(2));

    let engine = FakeEngine::start(&[("/events", Reply::error(400, "bad filter"))]).unwrap();
    let (docker, config) = pair(&engine);
    let begun = Instant::now();
    let Err(error) = events_established(&docker, &config, None, Duration::from_secs(30)).await
    else {
        panic!("a refused subscription was established");
    };
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.to_string().contains("bad filter"), "{error}");
    assert!(begun.elapsed() < Duration::from_secs(2));

    let engine = FakeEngine::start(&[("/events", Reply::not_found())]).unwrap();
    let (docker, config) = pair(&engine);
    let Err(error) = events_established(&docker, &config, None, Duration::from_secs(30)).await
    else {
        panic!("a refused subscription was established");
    };
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

#[cfg(all(feature = "tcp", target_os = "linux"))]
#[tokio::test]
async fn the_streaming_tcp_connection_has_keepalive_on() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stream = crate::stream::connect_tcp(&listener.local_addr().unwrap().to_string())
        .await
        .unwrap();
    let socket = socket2::SockRef::from(&stream);
    assert!(socket.keepalive().unwrap());
    assert_eq!(
        socket.tcp_keepalive_time().unwrap(),
        Duration::from_secs(15)
    );
    assert_eq!(
        socket.tcp_keepalive_interval().unwrap(),
        Duration::from_secs(15)
    );
    assert_eq!(socket.tcp_keepalive_retries().unwrap(), 9);
}
