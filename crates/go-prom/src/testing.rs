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

//! Test helpers over the registry, ports of the Go tests' `gatherSeries` and `seriesID`, behind the
//! `testing` feature, for a program's own collector tests.
//!
//! Extracted from swarm-scheduler-exporter/src/metrics/testing.rs at c9e79afa8810; the copy in
//! swarm-device-access/src/metrics/testing.rs at 8b4ab3a3e417 is the same file.

use std::collections::BTreeMap;
use std::sync::Arc;

use go_compat::quote::quote;

use super::{Collector, Registry, Value};

/// A series as `name{a="x",b="y"}`, label names sorted and values quoted as Go's `%q`.
#[must_use]
pub fn series_id(name: &str, labels: &[(&str, &str)]) -> String {
    let mut labels = labels.to_vec();
    labels.sort_unstable();
    let pairs: Vec<String> = labels
        .iter()
        .map(|(label, value)| format!("{label}={}", quote(value)))
        .collect();
    format!("{name}{{{}}}", pairs.join(","))
}

/// Registers `collector` in a fresh registry, gathers it, and returns every scalar series by
/// [`series_id`]. A histogram series fails the test: the Go helper reads gauge values only.
///
/// # Panics
///
/// When `collector` cannot be registered, or a gathered series is a histogram.
#[must_use]
pub fn gather_series(collector: Arc<dyn Collector>) -> BTreeMap<String, f64> {
    let registry = Registry::new();
    registry
        .register(collector)
        .unwrap_or_else(|error| panic!("register collector: {error}"));
    let mut series = BTreeMap::new();
    for family in registry.gather() {
        for gathered in family.series {
            let labels: Vec<(&str, &str)> = gathered
                .labels
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let Value::Scalar(value) = gathered.value else {
                panic!("{}: not a scalar series", family.name);
            };
            series.insert(series_id(&family.name, &labels), value);
        }
    }
    series
}
