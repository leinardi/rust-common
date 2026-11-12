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

//! bitgauge's generic loader tests, under bitgauge's profile: every variable lowercased whole, a
//! variable without `__` ignored, every list split on commas and trimmed, an empty list variable
//! an empty list, the file's quoted numbers and booleans parsed, `true` and `false` only, and
//! jiff's friendly durations. They prove the extraction keeps its behaviour; each names the test
//! it comes from.
//!
//! Ported from bitgauge/src/config/tests/{load,types}.rs at 9be3b0d20765, over a reduced schema
//! of bitgauge's keys.

mod common;

use std::path::Path;

use common::{Kind, Loaded, Schema, Value};
use config_layer::yaml::{self, Booleans, FileError, MissingFile};
use config_layer::{
    BoolSpelling, EmptyValue, EnvOverlay, ItemTrim, ListSplit, Origin, QuotedScalars, Source,
    Typing, Unseparated,
};
use jiff::SignedDuration;

const SCHEMA: Schema = Schema {
    fields: &[
        ("compat.networkquality.enabled", Kind::Bool),
        ("limits.max_streams_per_client", Kind::Int),
        ("limits.requests_per_minute", Kind::Int),
        ("limits.tracked_clients", Kind::Int),
        ("log.format", Kind::Text),
        ("log.level", Kind::Text),
        ("metrics.enabled", Kind::Bool),
        ("server.base_path", Kind::Text),
        ("server.cors.allowed_origins", Kind::TextList),
        ("server.display_name", Kind::Text),
        ("server.listen", Kind::Text),
        ("server.trusted_proxies", Kind::TextList),
        ("test_defaults.download.max_duration", Kind::Duration),
        ("test_defaults.overhead_compensation_percent", Kind::Number),
        ("test_defaults.phases", Kind::TextList),
        ("test_defaults.streams.fixed", Kind::Int),
        ("test_defaults.streams.max", Kind::Int),
    ],
};

fn typing() -> Typing {
    Typing::strict()
        .quoted_scalars(QuotedScalars::Parsed)
        .booleans(BoolSpelling::Exact)
}

fn overlay() -> EnvOverlay<'static> {
    EnvOverlay::new("BITGAUGE_", "__", Unseparated::Ignored, |path| {
        SCHEMA.field(&path.join(".")).is_some()
    })
    .lists(
        |path| SCHEMA.field(&path.join(".")) == Some(Kind::TextList),
        ListSplit::new(',', ItemTrim::Trimmed, EmptyValue::NoItems),
    )
}

#[expect(
    clippy::expect_used,
    reason = "a test helper: a broken fixture fails the test"
)]
fn load(text: &str, environment: &[&str]) -> Loaded {
    common::load(
        &SCHEMA,
        typing(),
        Booleans::AnyCase,
        &overlay(),
        text,
        environment,
    )
    .expect("valid YAML")
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn texts(values: &[&str]) -> Value {
    Value::TextList(values.iter().map(|&value| value.to_owned()).collect())
}

/// From: bitgauge `yaml_diagnostics_are_single_line_without_developer_hints`
#[test]
fn yaml_diagnostics_are_single_line_without_developer_hints() {
    for text in [
        "server:\n  display_name: one\n  display_name: two\n",
        "server: [\n",
        "server: {}\n---\nserver: {}\n",
        "- not\n- a mapping\n",
    ] {
        let message = yaml::parse(text, Booleans::AnyCase)
            .expect_err("invalid YAML")
            .to_string();
        assert!(!message.contains('\n'), "{message}");
        assert!(!message.contains("DuplicateKeyPolicy"), "{message}");
        assert!(!message.contains("from_str_multiple"), "{message}");
        assert!(!message.contains("display_name: one"), "{message}");
        assert!(!message.is_empty(), "a diagnostic explains the failure");
    }
    let duplicate = yaml::parse(
        "server:\n  display_name: one\n  display_name: two\n",
        Booleans::AnyCase,
    )
    .expect_err("a duplicate key");
    assert_eq!(
        duplicate.location().map(|location| location.line),
        Some(3),
        "a syntax problem keeps its location: {duplicate}"
    );
}

/// `BitGauge` reads `true` and `false` in any case as booleans (serde-saphyr's `strict_booleans`),
/// so a mixed-case value types as a boolean and is refused where text is expected.
#[test]
fn mixed_case_booleans_in_the_file_are_booleans() {
    let loaded = load("metrics:\n  enabled: tRuE\nlog:\n  level: FaLsE\n", &[]);
    assert_eq!(loaded.value("metrics.enabled"), Some(&Value::Bool(true)));
    assert_eq!(loaded.problem_paths(), ["log.level"]);
}

/// From: bitgauge `environment_mapping`
#[test]
fn environment_mapping() {
    let loaded = load(
        "",
        &[
            "BITGAUGE_SERVER__LISTEN=:9090",
            "BITGAUGE_SERVER__TRUSTED_PROXIES=172.30.0.2/32,fd00::2/128",
            "BITGAUGE_TEST_DEFAULTS__STREAMS__MAX=4",
            "BITGAUGE_COMPAT__NETWORKQUALITY__ENABLED=true",
            "BITGAUGE_SERVICE_HOST=10.43.0.12",
            "BITGAUGE_PORT=tcp://10.43.0.12:8080",
            "HOME=/root",
        ],
    );
    assert_eq!(loaded.problems, []);
    assert_eq!(loaded.value("server.listen"), Some(&text(":9090")));
    assert_eq!(
        loaded.value("server.trusted_proxies"),
        Some(&texts(&["172.30.0.2/32", "fd00::2/128"]))
    );
    assert_eq!(
        loaded.value("test_defaults.streams.max"),
        Some(&Value::Int(4))
    );
    assert_eq!(
        loaded.value("compat.networkquality.enabled"),
        Some(&Value::Bool(true))
    );
    assert_eq!(loaded.ignored, ["BITGAUGE_PORT", "BITGAUGE_SERVICE_HOST"]);
}

/// From: bitgauge `precedence`
#[test]
fn precedence() {
    let file = "
server:
  display_name: home
  base_path: /speed
limits:
  max_streams_per_client: 16
test_defaults:
  phases: [download]
";
    // The file sets only the keys it has.
    let loaded = load(file, &[]);
    assert_eq!(loaded.value("server.display_name"), Some(&text("home")));
    assert_eq!(
        loaded.value("limits.max_streams_per_client"),
        Some(&Value::Int(16))
    );
    assert_eq!(loaded.value("log.level"), None);

    // The environment overrides the file, key by key.
    let loaded = load(file, &["BITGAUGE_SERVER__DISPLAY_NAME=lab"]);
    assert_eq!(loaded.value("server.display_name"), Some(&text("lab")));
    assert_eq!(loaded.value("server.base_path"), Some(&text("/speed")));

    // The environment sets a key the file leaves alone.
    let loaded = load(
        file,
        &["BITGAUGE_LOG__LEVEL=debug", "BITGAUGE_LOG__FORMAT=json"],
    );
    assert_eq!(loaded.value("log.level"), Some(&text("debug")));
    assert_eq!(loaded.value("log.format"), Some(&text("json")));
    assert_eq!(loaded.value("server.display_name"), Some(&text("home")));

    // An empty list variable clears the list.
    let loaded = load(file, &["BITGAUGE_TEST_DEFAULTS__PHASES="]);
    assert_eq!(loaded.value("test_defaults.phases"), Some(&texts(&[])));

    // A comma list keeps its order and trims spaces.
    let loaded = load(
        file,
        &[
            "BITGAUGE_TEST_DEFAULTS__PHASES=upload, download",
            "BITGAUGE_SERVER__CORS__ALLOWED_ORIGINS=https://a.lan,http://b.lan:8080",
        ],
    );
    assert_eq!(
        loaded.value("test_defaults.phases"),
        Some(&texts(&["upload", "download"]))
    );
    assert_eq!(
        loaded.value("server.cors.allowed_origins"),
        Some(&texts(&["https://a.lan", "http://b.lan:8080"]))
    );
}

/// From: bitgauge `file_lists`
#[test]
fn file_lists() {
    let loaded = load(
        r#"
server:
  trusted_proxies: ["10.0.0.0/8", "::1/128"]
  cors:
    allowed_origins: [https://speed.example.lan]
test_defaults:
  phases: [download]
"#,
        &[],
    );
    assert_eq!(loaded.problems, []);
    assert_eq!(
        loaded.value("server.trusted_proxies"),
        Some(&texts(&["10.0.0.0/8", "::1/128"]))
    );
    assert_eq!(
        loaded.value("server.cors.allowed_origins"),
        Some(&texts(&["https://speed.example.lan"]))
    );
    assert_eq!(
        loaded.value("test_defaults.phases"),
        Some(&texts(&["download"]))
    );
}

/// From: bitgauge `file_paths`
#[test]
fn file_paths() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let missing = directory.path().join("missing.yaml");
    assert!(matches!(
        yaml::read_file(&missing, MissingFile::Refused, Booleans::AnyCase),
        Err(FileError::NotFound { .. })
    ));
    assert_eq!(
        yaml::read_file(&missing, MissingFile::Empty, Booleans::AnyCase)
            .expect("a missing default path"),
        config_layer::Map::new()
    );
    assert!(matches!(
        yaml::read_file(directory.path(), MissingFile::Refused, Booleans::AnyCase),
        Err(FileError::Read { .. })
    ));
    let readme = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    assert!(
        matches!(
            yaml::read_file(readme, MissingFile::Refused, Booleans::AnyCase),
            Err(FileError::Yaml { .. })
        ),
        "a file that is not YAML"
    );

    for invalid in [
        "server: [unclosed",
        "server:\n  listen: \":1\"\n  listen: \":2\"\n",
        "- a list\n",
        "just text\n",
    ] {
        assert!(
            yaml::parse(invalid, Booleans::AnyCase).is_err(),
            "{invalid:?} must not load"
        );
    }
    for empty in ["", "# only a comment\n", "~\n", "{}\n"] {
        assert_eq!(
            yaml::parse(empty, Booleans::AnyCase).expect("an empty configuration"),
            config_layer::Map::new(),
            "{empty:?} is empty"
        );
    }
}

/// From: bitgauge `environment_paths_are_checked_before_tree_insertion`
#[test]
fn environment_paths_are_checked_before_tree_insertion() {
    for (bad, good, key) in [
        (
            "BITGAUGE_SERVER__LISTEN__TYPO=x",
            "BITGAUGE_SERVER__LISTEN=127.0.0.1:8080",
            "server.listen.typo",
        ),
        (
            "BITGAUGE_SERVER__CORS=x",
            "BITGAUGE_SERVER__CORS__ALLOWED_ORIGINS=http://127.0.0.1",
            "server.cors",
        ),
        (
            "BITGAUGE_TEST_DEFAULTS__STREAMS=x",
            "BITGAUGE_TEST_DEFAULTS__STREAMS__MAX=4",
            "test_defaults.streams",
        ),
        (
            "BITGAUGE_SERVER__LISTN=x",
            "BITGAUGE_SERVER__LISTEN=127.0.0.1:8080",
            "server.listn",
        ),
        (
            "BITGAUGE_STORAGE__PATH=x",
            "BITGAUGE_SERVER__LISTEN=127.0.0.1:8080",
            "storage.path",
        ),
        (
            "BITGAUGE_SERVER__CORS__ALLOWED_ORIGINS__TYPO=x",
            "BITGAUGE_SERVER__CORS__ALLOWED_ORIGINS=http://127.0.0.1",
            "server.cors.allowed_origins.typo",
        ),
    ] {
        let forward = load("", &[bad, good, "BITGAUGE_LIMITS__TRACKED_CLIENTS=abc"]);
        let reverse = load("", &[good, bad, "BITGAUGE_LIMITS__TRACKED_CLIENTS=abc"]);
        assert_eq!(
            forward.problems, reverse.problems,
            "{bad}: diagnostic order"
        );
        assert_eq!(
            forward.problem_paths(),
            ["limits.tracked_clients", key],
            "{bad}: {:?}",
            forward.problems
        );
        assert_eq!(
            forward.problems[1].source,
            Source::Environment {
                variable: bad.split_once('=').expect("NAME=value").0.to_owned()
            },
            "{bad}: the offending variable"
        );
        assert!(forward.value(&good_key(good)).is_some(), "{good} is kept");
    }
}

#[expect(
    clippy::expect_used,
    reason = "a test helper: a broken fixture fails the test"
)]
fn good_key(entry: &str) -> String {
    let name = entry.split_once('=').expect("NAME=value").0;
    name.trim_start_matches("BITGAUGE_")
        .replace("__", ".")
        .to_lowercase()
}

/// From: bitgauge `valid_environment_siblings_keep_precedence_in_either_order`
#[test]
fn valid_environment_siblings_keep_precedence_in_either_order() {
    let entries = [
        "BITGAUGE_SERVER__LISTEN=127.0.0.1:8081",
        "BITGAUGE_SERVER__DISPLAY_NAME=lab",
        "BITGAUGE_TEST_DEFAULTS__PHASES=upload, download",
        "BITGAUGE_TEST_DEFAULTS__STREAMS__MAX=4",
        "BITGAUGE_TEST_DEFAULTS__STREAMS__FIXED=2",
        "BITGAUGE_PORT=ignored",
        "bitgauge_SERVER__LISTEN=ignored",
    ];
    let forward = load("server:\n  display_name: file\n", &entries);
    let reversed: Vec<&str> = entries.into_iter().rev().collect();
    let reverse = load("server:\n  display_name: file\n", &reversed);
    assert_eq!(forward, reverse);
    assert!(forward.problems.is_empty(), "{:?}", forward.problems);
    assert_eq!(
        forward.value("server.listen"),
        Some(&text("127.0.0.1:8081"))
    );
    assert_eq!(forward.value("server.display_name"), Some(&text("lab")));
    assert_eq!(
        forward.value("test_defaults.phases"),
        Some(&texts(&["upload", "download"]))
    );
    assert_eq!(forward.ignored, ["BITGAUGE_PORT"]);
}

/// From: bitgauge `unknown_key_beside_a_decode_error`
#[test]
fn unknown_key_beside_a_decode_error() {
    let loaded = load(
        "server:\n  lisen: \":8080\"\n  trusted_proxies: [[10.0.0.0/33]]\n",
        &[],
    );
    assert_eq!(
        loaded.problem_paths(),
        ["server.lisen", "server.trusted_proxies[0]"]
    );
    assert_eq!(loaded.problems[0].message, "unknown key");
    assert_eq!(loaded.problems[1].message, "expected text, got a list");
}

/// From: bitgauge `every_decode_error_is_reported`
#[test]
fn every_decode_error_is_reported() {
    let loaded = load(
        "limits:\n  max_streams_per_client: abc\n  requests_per_minute: xyz\n\
         server:\n  trusted_proxies: [{}]\n",
        &[],
    );
    assert_eq!(
        loaded.problem_paths(),
        [
            "limits.max_streams_per_client",
            "limits.requests_per_minute",
            "server.trusted_proxies[0]"
        ]
    );
    assert_eq!(loaded.problems[0].message, "invalid number: \"abc\"");
}

/// From: bitgauge `environment_list_item_reports_its_value`
#[test]
fn environment_list_item_reports_its_value() {
    let overlay = overlay().overlay([("BITGAUGE_SERVER__TRUSTED_PROXIES", "10.0.0.0/33")]);
    let node = &overlay.tree["server"].value;
    let config_layer::Raw::Map(server) = node else {
        panic!("server is a mapping: {node:?}");
    };
    let config_layer::Raw::List(items) = &server["trusted_proxies"].value else {
        panic!("a list: {server:?}");
    };
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].origin,
        Origin::Environment("BITGAUGE_SERVER__TRUSTED_PROXIES".to_owned())
    );
    assert_eq!(items[0].value.to_string(), "10.0.0.0/33");
}

/// From: bitgauge `value_syntax` (the generic rows: booleans, numbers, durations)
#[test]
fn value_syntax() {
    for (entry, key, want) in [
        (
            "BITGAUGE_METRICS__ENABLED=false",
            "metrics.enabled",
            Value::Bool(false),
        ),
        (
            "BITGAUGE_METRICS__ENABLED=true",
            "metrics.enabled",
            Value::Bool(true),
        ),
        (
            "BITGAUGE_TEST_DEFAULTS__OVERHEAD_COMPENSATION_PERCENT=5.13",
            "test_defaults.overhead_compensation_percent",
            Value::Number(5.13),
        ),
        (
            "BITGAUGE_TEST_DEFAULTS__DOWNLOAD__MAX_DURATION=2m",
            "test_defaults.download.max_duration",
            Value::Duration(SignedDuration::from_secs(120)),
        ),
        (
            "BITGAUGE_TEST_DEFAULTS__DOWNLOAD__MAX_DURATION=1m30s",
            "test_defaults.download.max_duration",
            Value::Duration(SignedDuration::from_secs(90)),
        ),
        (
            "BITGAUGE_TEST_DEFAULTS__DOWNLOAD__MAX_DURATION=1 minute",
            "test_defaults.download.max_duration",
            Value::Duration(SignedDuration::from_secs(60)),
        ),
    ] {
        let loaded = load("", &[entry]);
        assert_eq!(loaded.problems, [], "{entry}");
        assert_eq!(loaded.value(key), Some(&want), "{entry}");
    }
    for (entry, message) in [
        (
            "BITGAUGE_METRICS__ENABLED=TRUE",
            "invalid boolean: \"TRUE\" (want true or false)",
        ),
        (
            "BITGAUGE_METRICS__ENABLED=1",
            "invalid boolean: \"1\" (want true or false)",
        ),
        (
            "BITGAUGE_TEST_DEFAULTS__DOWNLOAD__MAX_DURATION=0",
            "\"0\" is not a duration such as 500ms, 10s or 2m",
        ),
        (
            "BITGAUGE_TEST_DEFAULTS__DOWNLOAD__MAX_DURATION=1d",
            "\"1d\" is not a duration such as 500ms, 10s or 2m",
        ),
    ] {
        let loaded = load("", &[entry]);
        assert_eq!(loaded.problems.len(), 1, "{entry}: {:?}", loaded.problems);
        assert_eq!(loaded.problems[0].message, message, "{entry}");
    }
}

/// bitgauge's list read checks the kind before any number: `.nan` for a list is "expected a
/// list" (bitgauge/src/config/decode.rs `list`).
#[test]
fn a_non_finite_number_for_a_list_is_the_wrong_kind() {
    let loaded = load("test_defaults:\n  phases: .nan\n", &[]);
    assert_eq!(loaded.problem_paths(), ["test_defaults.phases"]);
    assert_eq!(loaded.problems[0].message, "expected a list, got a number");
}

/// From: bitgauge `file_value_syntax`
#[test]
fn file_value_syntax() {
    let loaded = load(
        r#"
limits:
  max_streams_per_client: 8.0
  requests_per_minute: "600"
test_defaults:
  overhead_compensation_percent: 6
compat:
  networkquality:
    enabled: true
"#,
        &[],
    );
    assert_eq!(loaded.problems, []);
    assert_eq!(
        loaded.value("limits.max_streams_per_client"),
        Some(&Value::Int(8)),
        "a whole float is an integer"
    );
    assert_eq!(
        loaded.value("limits.requests_per_minute"),
        Some(&Value::Int(600)),
        "quoted digits are an integer"
    );
    assert_eq!(
        loaded.value("test_defaults.overhead_compensation_percent"),
        Some(&Value::Number(6.0))
    );
    assert_eq!(
        loaded.value("compat.networkquality.enabled"),
        Some(&Value::Bool(true))
    );
}

/// From: bitgauge `file_problems_keep_their_location`
#[test]
fn file_problems_keep_their_location() {
    let loaded = load(
        "server:\n  listen: 8080\n  lisen: x\n  trusted_proxies: [10.0.0.0/8, [bad]]\nlog:\n  level:\n",
        &["BITGAUGE_LIMITS__TRACKED_CLIENTS=abc"],
    );
    let lines: Vec<(&str, Option<u64>)> = loaded
        .problems
        .iter()
        .map(|problem| {
            (
                problem.path.as_str(),
                if let Source::Yaml { location, .. } = &problem.source {
                    Some(location.line)
                } else {
                    None
                },
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            ("limits.tracked_clients", None),
            ("log.level", Some(6)),
            ("server.lisen", Some(3)),
            ("server.listen", Some(2)),
            ("server.trusted_proxies[1]", Some(4)),
        ]
    );
}

/// From: bitgauge `duration_text_forms`
#[test]
fn duration_text_forms() {
    for (nanos, want) in [
        (0, "0s"),
        (750, "750ns"),
        (1500, "1µs 500ns"),
        (500_000_000, "500ms"),
        (10_000_000_000, "10s"),
        (90_000_000_000, "1m 30s"),
        (600_000_000_000, "10m"),
        (7_200_000_000_000, "2h"),
        (1_500_000_000, "1s 500ms"),
        (-1_500_000_000, "1s 500ms ago"),
    ] {
        assert_eq!(
            config_layer::text::duration(SignedDuration::from_nanos(nanos)),
            want
        );
    }
    assert_eq!(
        config_layer::text::duration(SignedDuration::from_hours(3_000_000)),
        "3000000h"
    );
}
