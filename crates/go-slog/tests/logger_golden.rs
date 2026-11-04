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

//! The logger against Go captures, byte for byte. Each directory under `tests/testdata` holds a
//! corpus of records (`corpus.json`) and what Go wrote for it through slog's text and JSON
//! handlers and one Go plain handler, each with and without the time
//! (`{text,json,plain}_{time,notime}.log`), all at 2026-10-25T14:03:04.567891234+01:00:
//!
//! - `quoted/`: Go 1.26, the plain handler of [`PlainStyle::Quoted`]; 54 records, three of which
//!   hold invalid UTF-8 (`"a\xffb"`, `"\xff"`, `[]string{"\xff"}`), which a Rust string cannot
//!   carry. Their corpus entries say only what Go logged (`invalid_utf8`), and the one line each
//!   wrote in every file is skipped, so 51 records per file are compared. Go logged these records
//!   from a program of its own; `corpus.json` is the same records written out as data.
//! - `raw/`: Go 1.26.9, the plain handler of [`PlainStyle::Raw`]; 54 records, written by the Go
//!   capture test together with `corpus.json`, which also holds how Go's `parseLevel` read each
//!   level spelling (`levels`). A plain record whose text holds a newline spans two lines.
//!
//! The corpus is read rather than rebuilt here, so the Rust side logs exactly the records the Go
//! side logged: each attribute carries its slog kind and its value as text (numbers as decimal
//! strings, durations in nanoseconds, times in RFC 3339 with their offset), and `with` lists the
//! `With` and `WithGroup` calls in order. `min_level` is the logger's minimum level (debug when
//! absent). Each record is logged on its own and must be the next bytes of the file; the file must
//! end with the last record.
//!
//! Extracted from swarm-device-access/tests/logger_golden.rs at 8b4ab3a3e417 (the corpus reader)
//! and swarm-scheduler-exporter/tests/logger_golden.rs at c9e79afa8810 (the records of `quoted/`,
//! written out as `quoted/corpus.json`).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use go_slog::{
    Attr, Config, Format, Level, Logger, MemorySink, PlainStyle, Time, Value, parse_level,
    parse_level_strict,
};
use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};
use serde_json::Value as Json;

/// Fails the test at the caller's line, for a malformed corpus: the helpers below run outside
/// `#[test]` functions, where `unwrap` is not allowed.
trait OrFail<T> {
    fn or_fail(self) -> T;
}

impl<T, E: std::fmt::Debug> OrFail<T> for Result<T, E> {
    #[track_caller]
    fn or_fail(self) -> T {
        self.unwrap_or_else(|error| panic!("{error:?}"))
    }
}

impl<T> OrFail<T> for Option<T> {
    #[track_caller]
    fn or_fail(self) -> T {
        self.unwrap_or_else(|| panic!("missing"))
    }
}

fn testdata(directory: &str, name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata")
        .join(directory)
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn corpus(directory: &str) -> Json {
    serde_json::from_str(&testdata(directory, "corpus.json")).or_fail()
}

fn text(json: &Json) -> &str {
    json.as_str()
        .unwrap_or_else(|| panic!("not a string: {json}"))
}

fn level(json: &Json) -> Level {
    Level(i32::try_from(json.as_i64().or_fail()).or_fail())
}

/// A Go `time.Time` in RFC 3339, with the offset it was written at.
fn time(rfc3339: &str) -> Time {
    let timestamp: Timestamp = rfc3339.parse().or_fail();
    let offset = if rfc3339.ends_with('Z') {
        Offset::UTC
    } else {
        let zone = &rfc3339[rfc3339.len() - 6..];
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let hours: i32 = zone[1..3].parse().or_fail();
        let minutes: i32 = zone[4..6].parse().or_fail();
        Offset::from_seconds(sign * (hours * 3600 + minutes * 60)).or_fail()
    };
    Time { timestamp, offset }
}

/// One corpus attribute, by its kind.
fn attr(json: &Json) -> Attr {
    let key = text(&json["key"]).to_owned();
    let value = &json["value"];
    let value = match text(&json["kind"]) {
        "string" => Value::Str(text(value).to_owned()),
        "stringer" => Value::Stringer(text(value).to_owned()),
        "error" => Value::Error(text(value).to_owned()),
        "int" => Value::Int(text(value).parse().or_fail()),
        "uint" => Value::Uint(text(value).parse().or_fail()),
        "float" => Value::Float(text(value).parse().or_fail()),
        "bool" => Value::Bool(value.as_bool().or_fail()),
        "duration" => Value::Duration(text(value).parse().or_fail()),
        "time" => Value::Time(time(text(value))),
        "strings" if value.is_null() => Value::NilStrings,
        "strings" => Value::Strings(
            value
                .as_array()
                .or_fail()
                .iter()
                .map(|item| text(item).to_owned())
                .collect(),
        ),
        "nil" => Value::Nil,
        "group" => return Attr::group(key, attrs(&json["attrs"])),
        kind => panic!("unknown kind {kind}"),
    };
    Attr::new(key, value)
}

/// A list of corpus attributes; absent is empty.
fn attrs(json: &Json) -> Vec<Attr> {
    json.as_array()
        .map(|items| items.iter().map(attr).collect())
        .unwrap_or_default()
}

/// Logs every record of the `directory` corpus through a `format` logger, each on its own, and
/// compares it with the next bytes of `file`.
fn compare(directory: &str, plain_style: PlainStyle, format: Format, include_time: bool) {
    let file = format!(
        "{}_{}.log",
        match format {
            Format::Text => "text",
            Format::Json => "json",
            Format::Plain => "plain",
        },
        if include_time { "time" } else { "notime" }
    );
    let want = testdata(directory, &file);
    let corpus = corpus(directory);
    let record_time = time(text(&corpus["time"]));
    let min_level = corpus.get("min_level").map_or(Level::DEBUG, level);

    let mut rest = want.as_str();
    let mut compared = 0;
    for (index, record) in corpus["records"].as_array().or_fail().iter().enumerate() {
        if record.get("invalid_utf8").is_some() {
            let line_end = rest.find('\n').map_or(rest.len(), |end| end + 1);
            rest = &rest[line_end..];
            continue;
        }
        let sink = MemorySink::default();
        let mut logger = Logger::new(
            Config {
                format,
                level: min_level,
                include_time,
                plain_style,
            },
            Box::new(sink.clone()),
            TimeZone::fixed(record_time.offset),
            Arc::new(move || record_time.timestamp),
        );
        for step in record["with"].as_array().into_iter().flatten() {
            logger = match step.get("group") {
                Some(name) => logger.with_group(text(name)),
                None => logger.with_attrs(&attrs(&step["attrs"])),
            };
        }
        logger.log(
            level(&record["level"]),
            text(&record["message"]),
            &attrs(&record["attrs"]),
        );
        let got = sink.contents();
        let Some(after) = rest.strip_prefix(got.as_str()) else {
            let expected = rest.split_inclusive('\n').next().unwrap_or_default();
            panic!(
                "{directory}/{file}: record {index} differs\n  Go   {expected:?}\n  Rust {got:?}"
            );
        };
        rest = after;
        compared += 1;
    }
    assert!(
        rest.is_empty(),
        "{directory}/{file}: Go wrote more: {rest:?}"
    );
    // Every record but the invalid-UTF-8 ones the quoted corpus skips (3 of its 54).
    let expected = match directory {
        "quoted" => 51,
        "raw" => 54,
        other => panic!("unknown corpus {other}"),
    };
    assert_eq!(compared, expected, "{directory}/{file}: records compared");
}

#[test]
fn quoted_text_matches_go() {
    compare("quoted", PlainStyle::Quoted, Format::Text, true);
    compare("quoted", PlainStyle::Quoted, Format::Text, false);
}

#[test]
fn quoted_json_matches_go() {
    compare("quoted", PlainStyle::Quoted, Format::Json, true);
    compare("quoted", PlainStyle::Quoted, Format::Json, false);
}

#[test]
fn quoted_plain_matches_go() {
    compare("quoted", PlainStyle::Quoted, Format::Plain, true);
    compare("quoted", PlainStyle::Quoted, Format::Plain, false);
}

#[test]
fn raw_text_matches_go() {
    compare("raw", PlainStyle::Raw, Format::Text, true);
    compare("raw", PlainStyle::Raw, Format::Text, false);
}

#[test]
fn raw_json_matches_go() {
    compare("raw", PlainStyle::Raw, Format::Json, true);
    compare("raw", PlainStyle::Raw, Format::Json, false);
}

#[test]
fn raw_plain_matches_go() {
    compare("raw", PlainStyle::Raw, Format::Plain, true);
    compare("raw", PlainStyle::Raw, Format::Plain, false);
}

/// The text and JSON formats do not depend on the plain style: each corpus's slog files match
/// under the other style too.
#[test]
fn slog_formats_ignore_the_plain_style() {
    for format in [Format::Text, Format::Json] {
        for include_time in [true, false] {
            compare("quoted", PlainStyle::Raw, format, include_time);
            compare("raw", PlainStyle::Quoted, format, include_time);
        }
    }
}

/// The level spellings of the corpus read as Go's `parseLevel` read them; the strict parser
/// agrees on every spelling it accepts, and refuses the rest.
#[test]
fn levels_match_go() {
    let corpus = corpus("raw");
    let cases = corpus["levels"].as_array().or_fail();
    assert!(!cases.is_empty());
    for case in cases {
        let input = text(&case["input"]);
        let want = level(&case["level"]);
        assert_eq!(parse_level(input), want, "parse_level({input:?})");
        match parse_level_strict(input) {
            Ok(strict) => assert_eq!(strict, want, "parse_level_strict({input:?})"),
            Err(_) => assert!(
                matches!(input, "" | "bogus"),
                "parse_level_strict({input:?}) refused a name"
            ),
        }
    }
}

/// The negative control: each plain file fails under the other style, so the comparison tells the
/// two Go handlers apart.
#[test]
fn plain_files_need_their_own_style() {
    for (directory, other) in [("quoted", PlainStyle::Raw), ("raw", PlainStyle::Quoted)] {
        for include_time in [true, false] {
            let result = std::panic::catch_unwind(|| {
                compare(directory, other, Format::Plain, include_time);
            });
            let Err(payload) = result else {
                panic!("{directory} matched under {other:?}");
            };
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or_default();
            assert!(
                message.contains("differs"),
                "{directory} under {other:?} failed for another reason: {message}"
            );
        }
    }
}
