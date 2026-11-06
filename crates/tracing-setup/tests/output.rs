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

//! What [`build`] writes, format by format and option by option.
//!
//! Ported from the logger tests of awtrix-controller/src/logger/mod.rs at afcd9ca26324 (the Go
//! `TestInit*` ones), swarm-rootless-docker/src/logger.rs at 3bb0fd8a615b and
//! bitgauge/src/logger/tests.rs at f9bb91eb5a50 (`TestNewToSelectsHandler`,
//! `TestNewToFiltersBelowLevel`); the target and span-field cases are new.

use tracing::Level;
use tracing::level_filters::LevelFilter;
use tracing_setup::testing::{Capture, set_default};
use tracing_setup::{Format, Options, build};

fn capture(
    format: Format,
    level: impl Into<LevelFilter>,
    options: Options,
    emit: impl FnOnce(),
) -> Capture {
    let capture = Capture::default();
    tracing::dispatcher::with_default(&build(capture.clone(), format, level, options), emit);
    capture
}

fn bitgauge() -> Options {
    Options::new().target(false).span_fields(false)
}

/// The default options are tracing-subscriber's: both shown.
#[test]
fn the_default_shows_the_target_and_the_span_fields() {
    assert_eq!(Options::default(), Options::new());
    assert!(Options::new().shows_target() && Options::new().shows_span_fields());
    let bitgauge = bitgauge();
    assert!(!bitgauge.shows_target() && !bitgauge.shows_span_fields());
}

/// The text line: an RFC 3339 UTC timestamp first, the level, the message, the fields as
/// `key=value`, no ANSI escapes.
#[test]
fn text_has_a_utc_timestamp_key_value_fields_and_no_ansi() {
    let text = capture(Format::Text, Level::INFO, Options::new(), || {
        tracing::info!(client_id = "abc", "broker: client connected");
    })
    .text();
    assert!(text.contains(" INFO "), "{text}");
    assert!(text.contains("broker: client connected"), "{text}");
    assert!(text.contains("client_id=\"abc\""), "{text}");
    assert!(!text.contains('\u{1b}'), "ANSI escapes in {text}");
    let (timestamp, _) = text.split_once(' ').expect("a timestamp first");
    assert!(
        timestamp.ends_with('Z') && timestamp.contains('T'),
        "an RFC 3339 UTC timestamp: {timestamp:?}"
    );
    assert_eq!(text.lines().count(), 1, "{text}");
}

/// The JSON record: one flat object per line, the fields beside the level, the message and a UTC
/// timestamp.
#[test]
fn json_is_one_flat_object_per_line() {
    let records = capture(Format::Json, Level::INFO, Options::new(), || {
        tracing::info!(daemon = "abc", "created the daemon");
        tracing::warn!("second");
    })
    .json_lines();
    assert_eq!(records.len(), 2, "{records:?}");
    let record = &records[0];
    assert_eq!(record["level"], "INFO");
    assert_eq!(record["message"], "created the daemon");
    assert_eq!(record["daemon"], "abc");
    assert!(record.get("fields").is_none(), "not flattened: {record}");
    assert!(
        record["timestamp"]
            .as_str()
            .is_some_and(|timestamp| timestamp.ends_with('Z')),
        "{record}"
    );
}

/// Go: `TestNewToFiltersBelowLevel`, `TestInitDebugEmitsDebugRecords`,
/// `TestInitInfoSuppressesDebugRecords`: each level lets through itself and above.
#[test]
fn the_level_filters() {
    for (level, emitted) in [
        (LevelFilter::TRACE, &["t", "d", "i", "w", "e"][..]),
        (LevelFilter::DEBUG, &["d", "i", "w", "e"]),
        (LevelFilter::INFO, &["i", "w", "e"]),
        (LevelFilter::WARN, &["w", "e"]),
        (LevelFilter::ERROR, &["e"]),
        (LevelFilter::OFF, &[]),
    ] {
        let messages: Vec<String> = capture(Format::Json, level, Options::new(), || {
            tracing::trace!("t");
            tracing::debug!("d");
            tracing::info!("i");
            tracing::warn!("w");
            tracing::error!("e");
        })
        .json_lines()
        .iter()
        .map(|record| record["message"].as_str().unwrap_or_default().to_owned())
        .collect();
        assert_eq!(messages, emitted, "level {level}");
    }
}

/// The target: shown by default in both formats, gone with `target(false)`.
#[test]
fn the_target_is_an_option() {
    let emit = || tracing::info!(target: "probe::module", "with a target");

    let shown = capture(Format::Text, Level::INFO, Options::new(), emit).text();
    assert!(shown.contains(" probe::module: with a target"), "{shown}");
    let hidden = capture(Format::Text, Level::INFO, bitgauge(), emit).text();
    assert!(!hidden.contains("probe::module"), "{hidden}");
    assert!(hidden.contains(" INFO with a target"), "{hidden}");

    let shown = capture(Format::Json, Level::INFO, Options::new(), emit).json_lines();
    assert_eq!(shown[0]["target"], "probe::module");
    let hidden = capture(Format::Json, Level::INFO, bitgauge(), emit).json_lines();
    assert!(hidden[0].get("target").is_none(), "{}", hidden[0]);
}

/// The JSON span fields: `span` and `spans` by default, neither with `span_fields(false)`; the
/// text format keeps its span prefix either way.
#[test]
fn the_json_span_fields_are_an_option() {
    let emit = || {
        let span = tracing::info_span!("request", id = 7);
        let _entered = span.enter();
        tracing::info!("inside");
    };

    let shown = capture(Format::Json, Level::INFO, Options::new(), emit).json_lines();
    assert_eq!(shown[0]["span"]["name"], "request", "{}", shown[0]);
    assert_eq!(shown[0]["span"]["id"], 7, "{}", shown[0]);
    assert_eq!(shown[0]["spans"][0]["name"], "request", "{}", shown[0]);
    let hidden = capture(Format::Json, Level::INFO, bitgauge(), emit).json_lines();
    assert!(
        hidden[0].get("span").is_none() && hidden[0].get("spans").is_none(),
        "{}",
        hidden[0]
    );
    assert_eq!(hidden[0]["message"], "inside");

    let text = capture(Format::Text, Level::INFO, bitgauge(), emit).text();
    assert!(text.contains("request{id=7}: inside"), "{text}");
}

/// `set_default` scopes the dispatch to the guard, and other threads keep their own default.
#[test]
fn set_default_lasts_as_long_as_the_guard() {
    let capture = Capture::default();
    let dispatch = build(capture.clone(), Format::Text, Level::INFO, Options::new());
    {
        let _guard = set_default(&dispatch);
        tracing::info!("kept");
        std::thread::spawn(|| tracing::info!("other thread"))
            .join()
            .expect("the thread runs");
    }
    tracing::info!("after the guard");
    let text = capture.text();
    assert!(text.contains("kept"), "{text}");
    assert!(
        !text.contains("other thread") && !text.contains("after the guard"),
        "{text}"
    );
}

/// What a test reads back: the bytes, the text and the JSON lines agree.
#[test]
fn the_capture_reads_back_what_was_written() {
    let capture = Capture::default();
    assert!(
        capture.bytes().is_empty() && capture.text().is_empty() && capture.json_lines().is_empty()
    );
    let mut writer = capture.clone();
    std::io::Write::write_all(&mut writer, b"{\"a\":1}\n{\"b\":2}\n").expect("written");
    std::io::Write::flush(&mut writer).expect("flushed");
    assert_eq!(capture.bytes(), b"{\"a\":1}\n{\"b\":2}\n");
    assert_eq!(capture.json_lines()[1]["b"], 2);
}

/// A line that is not JSON names itself.
#[test]
#[should_panic(expected = "not a JSON log line: \"plain text\"")]
fn a_line_that_is_not_json_is_named() {
    let mut writer = Capture::default();
    std::io::Write::write_all(&mut writer, b"plain text\n").expect("written");
    let _ = writer.json_lines();
}
