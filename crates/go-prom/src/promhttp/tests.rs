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

//! The promhttp handler metrics' tests.
//!
//! Extracted from swarm-scheduler-exporter/src/metrics/promhttp/tests.rs at c9e79afa8810 and
//! swarm-device-access/src/metrics/promhttp/tests.rs at 8b4ab3a3e417 (the Go capture read from
//! `runtime.prom`, the same bytes as the former's inline copy).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use super::{HandlerMetrics, sanitize_code};
use crate::process::ProcessCollector;
use crate::{Registry, text};

/// The `process_*` and `promhttp_*` lines of a Go `/metrics` answer (the Prometheus Go client
/// v1.24.1): every HELP and TYPE line, and the handler's samples after four scrapes, during the
/// fifth. The same bytes are swarm-scheduler-exporter's capture (v1.3.2,
/// `run1/go-v1.3.2-prometheus3.0.body`) and swarm-device-access's `TestGoldenRuntimeFamilies`.
fn go_runtime_families() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/promhttp/runtime.prom");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The family set, types and help texts match Go's capture byte for byte, and so do the handler
/// samples after the same four scrapes, seen from inside the fifth; the `go_*` families are not
/// served.
#[test]
fn runtime_families_match_the_go_capture() {
    let registry = Registry::new();
    registry
        .register(Arc::new(ProcessCollector::new()))
        .unwrap();
    let handler = HandlerMetrics::new();
    handler.register(&registry).unwrap();
    for _ in 0..4 {
        handler.begin().finish(200);
    }
    let _scrape = handler.begin();
    let mut out = String::new();
    text::encode(&mut out, &registry.gather());
    let mut kept = String::new();
    for line in out
        .lines()
        .filter(|line| line.starts_with('#') || line.starts_with("promhttp_"))
    {
        kept.push_str(line);
        kept.push('\n');
    }
    assert_eq!(kept, go_runtime_families());
}

/// The guard counts under the sanitised code on finish, and only lowers the gauge when dropped
/// unfinished (a handler that panicked, which Go does not count either).
#[test]
fn in_flight_guard_counts_on_finish_only() {
    let handler = HandlerMetrics::new();
    let first = handler.begin();
    let second = handler.begin();
    assert!((handler.in_flight.get() - 2.0).abs() < f64::EPSILON);
    first.finish(503);
    drop(second);
    assert!(handler.in_flight.get().abs() < f64::EPSILON);
    assert_eq!(handler.requests.get(&["503"]), Some(1.0));
    assert_eq!(handler.requests.get(&["200"]), Some(0.0));
    handler.begin().finish(0);
    handler.begin().finish(404);
    handler.begin().finish(600);
    assert_eq!(handler.requests.get(&["200"]), Some(1.0));
    assert_eq!(handler.requests.get(&["404"]), Some(1.0));
    assert_eq!(handler.requests.get(&["unknown"]), Some(1.0));
}

/// promhttp's `sanitizeCode`.
#[test]
fn sanitize_code_follows_promhttp() {
    for (status, want) in [
        (0, "200"),
        (99, "unknown"),
        (100, "100"),
        (200, "200"),
        (307, "307"),
        (431, "431"),
        (599, "599"),
        (600, "unknown"),
        (999, "unknown"),
    ] {
        assert_eq!(sanitize_code(status), want, "{status}");
    }
}

/// Registering twice is refused, as `prometheus.Register` would (promhttp reuses the existing
/// collectors there; a program that registers once, so it never reaches that path).
#[test]
fn registering_twice_is_refused() {
    let registry = Registry::new();
    let handler = HandlerMetrics::new();
    handler.register(&registry).unwrap();
    assert!(handler.register(&registry).is_err());
}
