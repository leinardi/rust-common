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

//! The timestamp toggle and [`build_reloadable`]; the cases are new.

use tracing::Level;
use tracing_setup::testing::{Capture, set_default};
use tracing_setup::{Format, Options, ReloadHandle, build, build_reloadable};

#[test]
fn time_off_leaves_the_timestamp_out_of_text_and_json() {
    assert!(Options::new().shows_time());
    assert!(!Options::new().time(false).shows_time());

    let text = Capture::default();
    let dispatch = build(
        text.clone(),
        Format::Text,
        Level::INFO,
        Options::new().time(false),
    );
    tracing::dispatcher::with_default(&dispatch, || tracing::info!("hello"));
    let text = text.text();
    assert!(text.trim_start().starts_with("INFO"), "{text:?}");

    let json = Capture::default();
    let dispatch = build(
        json.clone(),
        Format::Json,
        Level::INFO,
        Options::new().time(false),
    );
    tracing::dispatcher::with_default(&dispatch, || tracing::info!("hello"));
    let lines = json.json_lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].get("timestamp").is_none(), "{:?}", lines[0]);
    assert_eq!(lines[0]["message"], "hello");
}

#[test]
fn time_off_applies_to_a_reloadable_subscriber_in_both_formats() {
    let capture = Capture::default();
    let (dispatch, handle) = build_reloadable(
        capture.clone(),
        Format::Text,
        Level::INFO,
        Options::new().time(false),
    );
    let _guard = set_default(&dispatch);
    tracing::info!("text");
    handle.reconfigure(Format::Json, Level::INFO).unwrap();
    tracing::info!("json");
    let text = capture.text();
    let (first, rest) = text.split_once('\n').expect("two lines");
    assert!(first.trim_start().starts_with("INFO"), "{first:?}");
    let record: serde_json::Value = serde_json::from_str(rest.trim()).expect("JSON");
    assert!(record.get("timestamp").is_none());
}

#[test]
fn reload_switches_the_level() {
    let capture = Capture::default();
    let (dispatch, handle) =
        build_reloadable(capture.clone(), Format::Json, Level::INFO, Options::new());
    let _guard = set_default(&dispatch);

    tracing::debug!("hidden");
    tracing::info!("first");
    assert_eq!(messages(&capture), ["first"]);

    handle.set_level(Level::DEBUG).unwrap();
    tracing::debug!("shown");
    assert_eq!(messages(&capture), ["first", "shown"]);

    handle.reconfigure(Format::Json, Level::WARN).unwrap();
    tracing::info!("hidden again");
    assert_eq!(messages(&capture), ["first", "shown"]);
}

#[test]
fn reload_switches_the_format_on_the_same_dispatch() {
    let capture = Capture::default();
    let (dispatch, handle) =
        build_reloadable(capture.clone(), Format::Text, Level::INFO, Options::new());
    let _guard = set_default(&dispatch);

    tracing::info!("as text");
    handle.reconfigure(Format::Json, Level::INFO).unwrap();
    tracing::info!("as json");
    handle.set_format(Format::Text).unwrap();
    tracing::info!("text again");

    let text = capture.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text:?}");
    assert!(serde_json::from_str::<serde_json::Value>(lines[0]).is_err());
    assert!(lines[0].contains("as text"));
    let record: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(record["message"], "as json");
    assert!(record.get("timestamp").is_some());
    assert!(serde_json::from_str::<serde_json::Value>(lines[2]).is_err());
}

#[test]
fn a_span_opened_as_text_renders_as_json_after_the_switch() {
    let capture = Capture::default();
    let (dispatch, handle) = build_reloadable(
        capture.clone(),
        Format::Text,
        Level::INFO,
        Options::new().time(false),
    );
    let _guard = set_default(&dispatch);

    let span = tracing::info_span!("s", k = 1, late = tracing::field::Empty);
    let _entered = span.enter();
    tracing::info!("as text");
    handle.set_format(Format::Json).unwrap();
    span.record("late", 2);
    tracing::info!("as json");

    let text = capture.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    assert!(lines[0].contains("s{k=1}:"), "{:?}", lines[0]);
    let record: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(record["span"]["name"], "s");
    assert_eq!(record["span"]["k"], 1);
    assert_eq!(record["span"]["late"], 2);
    assert_eq!(record["spans"][0]["k"], 1);
}

#[test]
fn a_span_opened_as_json_renders_as_text_after_the_switch() {
    let capture = Capture::default();
    let (dispatch, handle) = build_reloadable(
        capture.clone(),
        Format::Json,
        Level::INFO,
        Options::new().time(false),
    );
    let _guard = set_default(&dispatch);

    let span = tracing::info_span!("s", k = 1, late = tracing::field::Empty);
    let _entered = span.enter();
    tracing::info!("as json");
    handle.set_format(Format::Text).unwrap();
    span.record("late", 2);
    tracing::info!("as text");

    let text = capture.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    let record: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(record["span"]["k"], 1);
    assert!(lines[1].contains("s{k=1 late=2}:"), "{:?}", lines[1]);
    assert!(lines[1].contains("as text"));
}

#[test]
fn the_handle_works_from_another_thread() {
    fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
    assert_send_sync_clone::<ReloadHandle>();

    let capture = Capture::default();
    let (dispatch, handle) =
        build_reloadable(capture.clone(), Format::Json, Level::INFO, Options::new());
    let _guard = set_default(&dispatch);

    let remote = handle.clone();
    std::thread::spawn(move || remote.reconfigure(Format::Json, Level::DEBUG).unwrap())
        .join()
        .unwrap();
    tracing::debug!("after the thread");
    assert_eq!(messages(&capture), ["after the thread"]);
}

#[test]
fn a_handle_outliving_its_subscriber_reports_an_error() {
    let (dispatch, handle) = build_reloadable(
        Capture::default(),
        Format::Text,
        Level::INFO,
        Options::new(),
    );
    drop(dispatch);
    // tracing's callsite registry upgrades its weak dispatcher references for a moment while
    // another test thread registers a callsite, so the subscriber can outlive the drop briefly:
    // the handle must report the error once that last reference is gone.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let error = loop {
        match handle.set_level(Level::DEBUG) {
            Err(error) => break error,
            Ok(()) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(()) => panic!("the handle still reached its subscriber 5s after the drop"),
        }
    };
    assert!(error.to_string().contains("could not be reconfigured"));
}

fn messages(capture: &Capture) -> Vec<String> {
    capture
        .json_lines()
        .iter()
        .map(|line| {
            line["message"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_default()
        })
        .collect()
}
