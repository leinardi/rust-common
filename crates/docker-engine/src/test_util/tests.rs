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

//! The test helpers on their own: the fake engine against the client on both transports, the
//! recorder's normalisation, and the small helpers, each with the failure it must report.

use std::time::Duration;

use serde::Deserialize;

use super::fake_engine::{FakeEngine, Listen, canonical_key, chunk, responder};
use super::recorder::{Target, canonical_filters, normalize, parse_query, split_version_prefix};
use super::{OrPanic, TempDir, eventually};
use crate::types::null_default;
use crate::{Client, Settings, StreamErrorKind};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Event {
    #[serde(rename = "Action", deserialize_with = "null_default")]
    action: String,
}

/// An engine that answers the ping, a list, and an event stream that ends as `end` says.
fn engine(end: &'static str) -> super::fake_engine::Responder {
    responder(move |mut connection| async move {
        while let Some(request) = connection.read_request().await {
            let open = match request.target.as_str() {
                "/_ping" => {
                    connection
                        .respond("200 OK", &[("Api-Version", "1.51")], b"OK", &request.method)
                        .await
                }
                "/v1.51/containers/json" => {
                    connection
                        .respond("200 OK", &[], b"[]", &request.method)
                        .await
                }
                target if target.starts_with("/v1.51/events") => {
                    let _ = connection
                        .start_chunked("200 OK", &[("Content-Type", "application/jsonl")])
                        .await
                        && connection.write_chunk(b"{\"Action\":\"start\"}\n").await;
                    match end {
                        "close" => {
                            let _ = connection.end_chunks().await;
                        }
                        "cut" => connection.shutdown().await,
                        _ => connection.hold_until_closed().await,
                    }
                    false
                }
                _ => {
                    connection
                        .respond("404 Not Found", &[], b"", &request.method)
                        .await
                }
            };
            if !open {
                return;
            }
        }
    })
}

/// Over a unix socket and TCP, the fake answers the client's requests on one kept-alive
/// connection, records them, and ends an event stream cleanly or cut.
#[tokio::test]
async fn the_fake_engine_serves_the_client() {
    let dir = TempDir::new("docker-engine-fake");
    let unix = FakeEngine::unix(&dir.path().join("d.sock"), engine("close")).or_panic();
    let tcp = FakeEngine::tcp("127.0.0.1:0", engine("cut"))
        .await
        .or_panic();
    assert!(matches!(unix.listen(), Listen::Unix(_)));
    assert!(matches!(tcp.listen(), Listen::Tcp(_)));
    for (engine, end) in [
        (&unix, StreamErrorKind::Eof),
        (&tcp, StreamErrorKind::UnexpectedEof),
    ] {
        let host = engine.host();
        let client = match host.strip_prefix("unix://") {
            Some(socket) => Client::new(Settings::unix(socket).or_panic()),
            #[cfg(feature = "tcp")]
            None => Client::new(
                Settings::from_env(|name| (name == "DOCKER_HOST").then(|| host.clone())).or_panic(),
            ),
            #[cfg(not(feature = "tcp"))]
            None => continue,
        };
        let listed: Vec<Event> = client.get_list("/containers/json", "").await.or_panic();
        assert!(listed.is_empty());
        let mut events = client.events("since=1").await.or_panic();
        assert_eq!(events.next::<Event>().await.or_panic().action, "start");
        assert_eq!(events.next::<Event>().await.or_panic_err().kind(), end);
        let requests = engine.requests();
        let wire: Vec<String> = requests
            .iter()
            .map(|request| format!("{} {}", request.method, request.target))
            .collect();
        assert_eq!(
            wire,
            [
                "HEAD /_ping",
                "GET /v1.51/containers/json",
                "GET /v1.51/events?since=1"
            ],
            "{host}"
        );
        assert!(
            requests.iter().all(|request| request.conn == 1),
            "{requests:?}"
        );
        assert_eq!(requests[2].header("accept").len(), 3);
        assert_eq!(requests[0].first_header("x-missing"), "");
    }
}

/// An event stream held open ends only when the client goes away.
#[tokio::test]
async fn a_held_stream_waits_for_the_client() {
    let dir = TempDir::new("docker-engine-fake");
    let engine = FakeEngine::unix(&dir.path().join("d.sock"), engine("hold")).or_panic();
    let client =
        Client::new(Settings::unix(&dir.path().join("d.sock").display().to_string()).or_panic());
    let mut events = client.events("since=1").await.or_panic();
    assert_eq!(events.next::<Event>().await.or_panic().action, "start");
    let waited = tokio::time::timeout(Duration::from_millis(100), events.next::<Event>()).await;
    assert!(waited.is_err(), "a held stream ended by itself");
    drop(events);
    assert_eq!(engine.requests().len(), 2);
}

/// A request body is read by its `Content-Length`, so the next request on the connection is
/// read whole.
#[tokio::test]
async fn request_bodies_are_read_by_their_length() {
    use tokio::io::AsyncWriteExt as _;

    let dir = TempDir::new("docker-engine-fake");
    let socket = dir.path().join("d.sock");
    let engine = FakeEngine::unix(
        &socket,
        responder(|mut connection| async move {
            while let Some(request) = connection.read_request().await {
                if !connection
                    .respond("200 OK", &[], b"", &request.method)
                    .await
                {
                    return;
                }
            }
        }),
    )
    .or_panic();
    let mut stream = tokio::net::UnixStream::connect(&socket).await.or_panic();
    stream
        .write_all(
            b"POST /a HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\n\r\nbodyGET /b HTTP/1.1\r\nHost: \
              x\r\n\r\n",
        )
        .await
        .or_panic();
    eventually("two requests", Duration::from_secs(5), || {
        engine.requests().len() == 2
    })
    .await;
    let requests = engine.requests();
    assert_eq!(requests[0].body, b"body");
    assert_eq!(requests[1].target, "/b");
    assert_eq!(requests[1].headers, [("Host".to_owned(), "x".to_owned())]);
}

#[test]
fn chunks_and_header_keys_are_go_s() {
    assert_eq!(chunk(b"hello"), b"5\r\nhello\r\n");
    assert_eq!(chunk(&[b'x'; 26])[..4], *b"1a\r\n");
    assert!(chunk(b"").is_empty());
    assert_eq!(canonical_key("content-length"), "Content-Length");
    assert_eq!(canonical_key("X-REGISTRY-AUTH"), "X-Registry-Auth");
}

/// The recorder's normalisation: the version prefix, Go's query decoding, both filter
/// encodings, and the times.
#[test]
fn targets_are_normalised_as_the_go_recorder_does() {
    assert_eq!(
        split_version_prefix("/v1.56/events"),
        ("/v1.56".to_owned(), "/events".to_owned())
    );
    for unversioned in ["/_ping", "/v1/x", "/vx.1/x", "/v1.56"] {
        assert_eq!(
            split_version_prefix(unversioned),
            (String::new(), unversioned.to_owned())
        );
    }
    assert_eq!(
        parse_query("a=1&&b=x+y%2Fz&c&bad=%zz"),
        [
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "x y/z".to_owned()),
            ("c".to_owned(), String::new()),
            ("bad".to_owned(), "%zz".to_owned()),
        ]
    );
    assert_eq!(
        canonical_filters(r#"{"type":{"service":true,"node":true}}"#),
        canonical_filters(r#"{"type":["node","service"]}"#)
    );
    assert_eq!(
        canonical_filters(r#"{"type":["node","service"],"a":[1]}"#),
        "{a:[<invalid>];type:[node,service]}"
    );
    assert_eq!(canonical_filters("[]"), "<invalid filters []>");
    let target = Target::parse(
        "/v1.51/events?since=1.5&filters=%7B%22type%22%3A%5B%22node%22%5D%7D&until=2",
    );
    assert_eq!(target.version_prefix, "/v1.51");
    assert_eq!(target.path, "/events");
    assert_eq!(target.query_value("since"), Some("1.5"));
    assert_eq!(target.query_value("missing"), None);
    assert_eq!(
        target.normalized("GET"),
        "GET /events?filters={type:[node]}&since=<time>&until=<time>"
    );
    assert_eq!(normalize("HEAD", "/_ping", &[]), "HEAD /_ping");
    let encoded = Target::parse("/v1.56/containers/a%20b/json");
    assert_eq!(encoded.raw_path, "/v1.56/containers/a b/json");
    assert_eq!(encoded.raw_query, "");
}

#[test]
fn or_panic_unwraps_or_names_the_failure() {
    assert_eq!(Ok::<_, String>(3).or_panic(), 3);
    assert_eq!(Err::<(), _>("no".to_owned()).or_panic_err(), "no");
    let failed = std::panic::catch_unwind(|| Err::<(), _>("boom".to_owned()).or_panic());
    let message = failed.unwrap_err();
    assert_eq!(
        message.downcast_ref::<String>().map(String::as_str),
        Some("unexpected error: boom")
    );
    let succeeded = std::panic::catch_unwind(|| Ok::<_, String>(7).or_panic_err());
    assert_eq!(
        succeeded
            .unwrap_err()
            .downcast_ref::<String>()
            .map(String::as_str),
        Some("unexpected success: 7")
    );
}

#[test]
fn a_temp_dir_goes_on_drop() {
    let dir = TempDir::new("docker-engine-temp");
    let path = dir.path().to_owned();
    assert!(path.is_dir());
    assert_eq!(dir.str(), path.to_str().unwrap());
    std::fs::write(path.join("f"), b"x").unwrap();
    drop(dir);
    assert!(!path.exists());
    assert_ne!(
        TempDir::new("docker-engine-temp").path(),
        TempDir::new("docker-engine-temp").path()
    );
}

/// A condition that never holds fails the test once the time is up, under a paused clock too.
#[tokio::test(start_paused = true)]
async fn eventually_fails_once_the_time_is_up() {
    let mut checks = 0;
    eventually("the third check", Duration::from_secs(1), || {
        checks += 1;
        checks == 3
    })
    .await;
    let never = tokio::spawn(eventually("never", Duration::from_millis(50), || false));
    let failure = never.await.unwrap_err().into_panic();
    assert_eq!(
        failure.downcast_ref::<String>().map(String::as_str),
        Some("timed out after 50ms waiting for never")
    );
}
