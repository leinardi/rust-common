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

//! One-shot stats and the raw service spec against the fake engine.

use std::time::Duration;

use crate::test_util::{FakeEngine, Reply};
use crate::{CancellationToken, Config, ErrorKind};

const SECS: Duration = Duration::from_secs(5);

fn config_of(engine: &FakeEngine) -> Config {
    Config::unix(engine.socket().to_str().unwrap())
        .api_version("1.44")
        .unwrap()
}

#[cfg(feature = "stats")]
mod stats {
    use super::*;
    use crate::stats::stats_once;

    /// An answer with every field runhold's Go formulas read, then trailing data that Go's
    /// `json.Decoder` would not read either.
    const SAMPLE: &str = r#"{"name":"/web","id":"c1","read":"2026-11-12T10:00:01Z","preread":"2026-11-12T10:00:00Z",
        "cpu_stats":{"cpu_usage":{"total_usage":2000,"percpu_usage":[1000,1000]},"system_cpu_usage":90000,"online_cpus":2},
        "precpu_stats":{"cpu_usage":{"total_usage":1000},"system_cpu_usage":80000,"online_cpus":2},
        "memory_stats":{"usage":5000,"limit":9000,"stats":{"inactive_file":1200,"total_inactive_file":700}},
        "future_field":{"x":1}}
        {"name":"/second"}"#;

    /// The request is moby's (`stream=false`, no `one-shot`), and the previous sample is decoded.
    #[tokio::test]
    async fn stats_once_asks_for_two_cycles_and_reads_the_previous_sample() {
        let engine = FakeEngine::start(&[("/containers/c1/stats", Reply::ok(SAMPLE))]).unwrap();
        let config = config_of(&engine);
        let stats = stats_once(&config, " c1 ", SECS, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(stats.name.as_deref(), Some("/web"));
        let cpu = stats.cpu_stats.unwrap();
        assert_eq!(cpu.cpu_usage.unwrap().total_usage, Some(2000));
        assert_eq!(
            (cpu.system_cpu_usage, cpu.online_cpus),
            (Some(90000), Some(2))
        );
        let previous = stats.precpu_stats.unwrap();
        assert_eq!(previous.cpu_usage.unwrap().total_usage, Some(1000));
        assert_eq!(previous.system_cpu_usage, Some(80000));
        let memory = stats.memory_stats.unwrap();
        assert_eq!((memory.usage, memory.limit), (Some(5000), Some(9000)));
        let map = memory.stats.unwrap();
        assert_eq!(
            (map.get("inactive_file"), map.get("total_inactive_file")),
            (Some(&1200), Some(&700))
        );
        let requests = engine.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].raw_path(),
            "/v1.44/containers/c1/stats?stream=false"
        );
    }

    /// A 404, an empty ID and an answer that is not JSON are errors of their kind; a stalled body
    /// times out.
    #[tokio::test]
    async fn stats_once_failures_keep_their_kind() {
        let engine = FakeEngine::start(&[
            ("/containers/odd/stats", Reply::ok("not json")),
            ("/containers/slow/stats", Reply::Stalled),
        ])
        .unwrap();
        let config = config_of(&engine);
        let cancel = CancellationToken::new();
        let error = stats_once(&config, "gone", SECS, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        let error = stats_once(&config, "", SECS, &cancel).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid container name or ID: value is empty"),
            "{error}"
        );
        let error = stats_once(&config, "odd", SECS, &cancel).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        let error = stats_once(&config, "slow", Duration::from_millis(200), &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
    }
}

#[cfg(feature = "raw-spec")]
mod raw_spec {
    use serde_json::{Value, json};

    use super::*;
    use crate::raw_spec::{inspect_service_raw, update_service_raw};

    /// A service with fields no model knows at three depths, nested arrays and an index near
    /// `u64::MAX`.
    fn service() -> Value {
        json!({
            "ID": "s1",
            "Version": {"Index": 18_446_744_073_709_551_000_u64},
            "CreatedAt": "2026-11-12T10:00:00.123456789Z",
            "Spec": {
                "Name": "web",
                "Labels": {"com.example": "x"},
                "TopLevelFuture": {"enabled": true},
                "TaskTemplate": {
                    "ContainerSpec": {
                        "Image": "nginx:1.27@sha256:0000000000000000000000000000000000000000000000000000000000000000",
                        "Healthcheck": {"Test": ["CMD", "true"], "Interval": 30_000_000_000_u64},
                        "Mounts": [{"Type": "bind", "Source": "/a", "Target": "/b", "Future": [[1, 2], [3]]}],
                        "DeepFuture": {"a": {"b": [null, false, -1, 1.5]}}
                    },
                    "Placement": {"Constraints": ["node.role==manager"]},
                    "ForceUpdate": 4,
                    "TaskFuture": "kept"
                },
                "Mode": {"Replicated": {"Replicas": 2}}
            },
            "Endpoint": {"Spec": {}}
        })
    }

    /// inspect, `ForceUpdate + 1`, update: the engine receives the spec it sent, every field kept
    /// and only `ForceUpdate` changed, under the exact version; the warnings come back.
    #[tokio::test]
    async fn a_spec_survives_inspect_and_update_with_every_field() {
        let engine = FakeEngine::start(&[
            ("/services/s1", Reply::ok(&service().to_string())),
            (
                "/services/s1/update",
                Reply::ok(r#"{"Warnings":["image could not be accessed"]}"#),
            ),
        ])
        .unwrap();
        let config = config_of(&engine);
        let cancel = CancellationToken::new();
        let raw = inspect_service_raw(&config, "s1", SECS, &cancel)
            .await
            .unwrap();
        assert_eq!(raw.version, 18_446_744_073_709_551_000);
        assert_eq!(raw.value, service());
        let mut spec = raw.spec().unwrap().clone();
        let force = &mut spec["TaskTemplate"]["ForceUpdate"];
        *force = json!(force.as_u64().unwrap() + 1);
        let warnings = update_service_raw(&config, " s1 ", raw.version, &spec, SECS, &cancel)
            .await
            .unwrap();
        assert_eq!(warnings, ["image could not be accessed"]);

        let requests = engine.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            (requests[0].method.as_str(), requests[0].raw_path()),
            ("GET", "/v1.44/services/s1")
        );
        assert_eq!(
            (requests[1].method.as_str(), requests[1].raw_path()),
            (
                "POST",
                "/v1.44/services/s1/update?version=18446744073709551000"
            )
        );
        assert_eq!(requests[1].header("content-type"), Some("application/json"));
        assert_eq!(requests[1].header("x-registry-auth"), None);
        let sent: Value = serde_json::from_slice(&requests[1].body).unwrap();
        let mut want = service()["Spec"].clone();
        want["TaskTemplate"]["ForceUpdate"] = json!(5);
        assert_eq!(sent, want);
    }

    /// An answer as Go's encoder writes it (`\u0026` and `\u003c` escapes, a `.0` float, an
    /// exponent, unknown fields at three depths), followed by trailing data Go's decoder never
    /// reads, parses to the values it means, and the spec sent back re-parses to the same.
    #[tokio::test]
    async fn a_go_encoded_answer_keeps_its_values() {
        let answer = r#"{"ID":"s1","Version":{"Index":42},"Spec":{"Name":"web","Future":{"Ratio":2.0,"Big":1e+21},"TaskTemplate":{"Placement":{"Constraints":["node.labels.a==x\u0026y","node.labels.b!=\u003cz\u003e"]},"ContainerSpec":{"Image":"nginx","Deeper":{"Deepest":[[],{"k":null}]}},"ForceUpdate":0}}}
garbage"#;
        let engine = FakeEngine::start(&[
            ("/services/s1", Reply::ok(answer)),
            ("/services/s1/update", Reply::ok("{}\nmore")),
        ])
        .unwrap();
        let config = config_of(&engine);
        let cancel = CancellationToken::new();
        let raw = inspect_service_raw(&config, "s1", SECS, &cancel)
            .await
            .unwrap();
        assert_eq!(raw.version, 42);
        let spec = raw.spec().unwrap().clone();
        assert_eq!(
            spec,
            json!({"Name": "web", "Future": {"Ratio": 2.0, "Big": 1e21},
                "TaskTemplate": {"Placement": {"Constraints": ["node.labels.a==x&y", "node.labels.b!=<z>"]},
                    "ContainerSpec": {"Image": "nginx", "Deeper": {"Deepest": [[], {"k": null}]}},
                    "ForceUpdate": 0}})
        );
        assert!(spec["Future"]["Ratio"].is_f64());
        let warnings = update_service_raw(&config, "s1", raw.version, &spec, SECS, &cancel)
            .await
            .unwrap();
        assert!(warnings.is_empty());
        let sent: Value = serde_json::from_slice(&engine.requests()[1].body).unwrap();
        assert_eq!(sent, spec);
        assert!(sent["Future"]["Ratio"].is_f64());
    }

    /// A 409 is a conflict (Docker 29 answers a stale version with a 500, `update out of
    /// sequence`, which stays `Other`); a missing service is not found; `null` warnings are none.
    #[tokio::test]
    async fn update_refusals_keep_their_kind() {
        let engine = FakeEngine::start(&[
            ("/services/busy/update", Reply::error(409, "conflict")),
            (
                "/services/stale/update",
                Reply::error(
                    500,
                    "rpc error: code = Unknown desc = update out of sequence",
                ),
            ),
            ("/services/quiet/update", Reply::ok(r#"{"Warnings":null}"#)),
        ])
        .unwrap();
        let config = config_of(&engine);
        let cancel = CancellationToken::new();
        let spec = json!({"Name": "x"});
        let error = update_service_raw(&config, "busy", 3, &spec, SECS, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict);
        let error = update_service_raw(&config, "stale", 3, &spec, SECS, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(
            error.to_string().contains("update out of sequence"),
            "{error}"
        );
        let error = update_service_raw(&config, "gone", 3, &spec, SECS, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        let warnings = update_service_raw(&config, "quiet", 3, &spec, SECS, &cancel)
            .await
            .unwrap();
        assert!(warnings.is_empty());
        let error = update_service_raw(&config, "\t", 3, &spec, SECS, &cancel)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid service name or ID: value is empty"),
            "{error}"
        );
    }

    /// An inspect answer without an unsigned `Version.Index` is an error, not version 0.
    #[tokio::test]
    async fn an_inspect_without_a_version_index_is_an_error() {
        let engine = FakeEngine::start(&[
            ("/services/none", Reply::ok(r#"{"ID":"none","Spec":{}}"#)),
            ("/services/neg", Reply::ok(r#"{"Version":{"Index":-1}}"#)),
            ("/services/text", Reply::ok(r#"{"Version":{"Index":"7"}}"#)),
        ])
        .unwrap();
        let config = config_of(&engine);
        let cancel = CancellationToken::new();
        for id in ["none", "neg", "text"] {
            let error = inspect_service_raw(&config, id, SECS, &cancel)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Other, "{id}: {error}");
        }
        let error = inspect_service_raw(&config, "gone", SECS, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }
}
