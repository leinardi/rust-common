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

//! `install_log_bridge` routes the `log` facade into the dispatcher, and the record keeps its own
//! target: tracing-subscriber's `tracing-log` feature, which `log-bridge` turns on, is what makes
//! the fmt layer print it instead of the bridge's `log`. A test binary of its own, since the `log`
//! logger is process-wide.
//!
//! Ported from awtrix-controller/src/logger/mod.rs at afcd9ca26324 (`log_facade_is_bridged`); the
//! target case is new.

use tracing::Level;
use tracing_setup::testing::Capture;
use tracing_setup::{Format, Options, build, install_log_bridge};

#[test]
fn the_log_facade_is_bridged_with_its_own_target() {
    install_log_bridge().expect("the first install");
    assert!(install_log_bridge().is_err(), "a second bridge");
    log::set_max_level(log::LevelFilter::Trace);

    let text = Capture::default();
    tracing::dispatcher::with_default(
        &build(text.clone(), Format::Text, Level::INFO, Options::new()),
        || {
            log::info!(target: "probe::dependency", "from the log facade");
            log::debug!("below the level");
        },
    );
    let text = text.text();
    assert!(
        text.contains(" probe::dependency: from the log facade"),
        "{text}"
    );
    assert!(!text.contains("below the level"), "{text}");
    assert!(!text.contains("log.target"), "unnormalised: {text}");

    let json = Capture::default();
    tracing::dispatcher::with_default(
        &build(json.clone(), Format::Json, Level::INFO, Options::new()),
        || log::warn!(target: "probe::dependency", "as JSON"),
    );
    let records = json.json_lines();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["target"], "probe::dependency", "{}", records[0]);
    assert_eq!(records[0]["message"], "as JSON");
    // tracing-subscriber's JSON also keeps the bridge's own fields, as awtrix-controller's output
    // has them; the text format drops them.
    assert_eq!(
        records[0]["log.target"], "probe::dependency",
        "{}",
        records[0]
    );
}
