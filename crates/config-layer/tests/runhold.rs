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

//! Runhold's profile: Go's koanf loader with the conversions runhold keeps (its difference 14)
//! and its environment mapping (`internal/config/load.go`: map paths keep their key's case, only
//! two keys split, untrimmed, a variable without `__` mapped). Every Go expectation here comes
//! from a Go run (go1.25.5): `strconv.ParseInt(s, 0, 64)`, `strconv.ParseBool`,
//! `strings.Split(s, ",")`, and koanf v2.4.1's `expandDurationDays` followed by
//! `time.ParseDuration`.

mod common;

use common::{Kind, Loaded, Schema, Value};
use config_layer::yaml::Booleans;
use config_layer::{
    BareZero, BoolSpelling, DayUnit, DurationSyntax, EmptyValue, EnvOverlay, IntegerSyntax,
    IntegerText, ItemTrim, ListSplit, Map, Node, Origin, Raw, ScalarLists, Typing, Unseparated,
};
use jiff::SignedDuration;

const SCHEMA: Schema = Schema {
    fields: &[
        ("auth.reverse_proxy.role_map", Kind::TextMap),
        ("maintenance.digest_refresh.concurrency", Kind::Int),
        ("maintenance.digest_refresh.include_labels", Kind::TextMap),
        ("maintenance.prune.nodes", Kind::TextList),
        ("audit.retention", Kind::Duration),
        ("server.listen", Kind::Text),
        ("server.trusted_proxies", Kind::TextList),
        ("terminal.default_shells", Kind::TextList),
        ("terminal.enabled", Kind::Bool),
        ("terminal.max_idle", Kind::Duration),
        ("debug", Kind::Bool),
    ],
};

fn typing() -> Typing {
    Typing::strict()
        .booleans(BoolSpelling::GoParseBool)
        // Runhold reads an environment integer as decimal, refusing Go's other bases (finding 33).
        .integers(IntegerSyntax::Decimal)
        .integer_text(IntegerText::Decimal)
        .scalar_lists(ScalarLists::Lifted)
        .durations(DurationSyntax::new(
            BareZero::Zero,
            DayUnit::TwentyFourHours,
        ))
}

const GO_SPLIT: ListSplit = ListSplit::new(',', ItemTrim::Untrimmed, EmptyValue::OneEmptyItem);

const BITGAUGE_SPLIT: ListSplit = ListSplit::new(',', ItemTrim::Trimmed, EmptyValue::NoItems);

/// Go's `envMapFields` and `envLabelMapFields`.
fn is_map_path(path: &[&str]) -> bool {
    matches!(path.last(), Some(&("include_labels" | "exclude_labels")))
        || matches!(
            path.join(".").as_str(),
            "auth.reverse_proxy.role_map" | "compose_projects"
        )
}

fn overlay(known: impl Fn(&[&str]) -> bool + Send + Sync + 'static) -> EnvOverlay<'static> {
    EnvOverlay::new("RUNHOLD_", "__", Unseparated::Mapped, known)
        .map_paths(is_map_path)
        .lists(
            |path| {
                matches!(
                    path.join(".").as_str(),
                    "terminal.default_shells" | "server.trusted_proxies"
                )
            },
            GO_SPLIT,
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
        Booleans::GoYamlV3,
        &overlay(|path| SCHEMA.knows(path)),
        text,
        environment,
    )
    .expect("valid YAML")
}

fn texts(values: &[&str]) -> Value {
    Value::TextList(values.iter().map(|&value| value.to_owned()).collect())
}

fn hours(count: i64) -> Value {
    Value::Duration(SignedDuration::from_secs(count * 3600))
}

/// Every form difference 14 keeps, from the file and from the environment.
#[test]
#[expect(clippy::too_many_lines, reason = "one row per form")]
fn difference_14_kept_forms() {
    let file_rows: &[(&str, &str, Value)] = &[
        // A scalar lifted into a one-element list (`nodes: all`).
        (
            "maintenance:\n  prune:\n    nodes: all\n",
            "maintenance.prune.nodes",
            texts(&["all"]),
        ),
        // An integer read as its decimal text.
        (
            "server:\n  listen: 8080\n",
            "server.listen",
            Value::Text("8080".to_owned()),
        ),
        (
            "server:\n  listen: 010\n",
            "server.listen",
            Value::Text("8".to_owned()),
        ),
        // A bare zero duration, as the integer and as the text.
        (
            "terminal:\n  max_idle: 0\n",
            "terminal.max_idle",
            Value::Duration(SignedDuration::ZERO),
        ),
        (
            "terminal:\n  max_idle: \"0\"\n",
            "terminal.max_idle",
            Value::Duration(SignedDuration::ZERO),
        ),
        // The day unit.
        ("audit:\n  retention: 30d\n", "audit.retention", hours(720)),
        ("audit:\n  retention: 1.5d\n", "audit.retention", hours(36)),
        (
            "audit:\n  retention: 7d12h\n",
            "audit.retention",
            hours(180),
        ),
        (
            "audit:\n  retention: 1h30m\n",
            "audit.retention",
            Value::Duration(SignedDuration::from_secs(5400)),
        ),
        // A whole float is an integer (mapstructure converts it).
        (
            "maintenance:\n  digest_refresh:\n    concurrency: 2.0\n",
            "maintenance.digest_refresh.concurrency",
            Value::Int(2),
        ),
    ];
    for (text, key, want) in file_rows {
        let loaded = load(text, &[]);
        assert_eq!(loaded.problems, [], "{text:?}");
        assert_eq!(loaded.value(key), Some(want), "{text:?}");
    }

    let mut env_rows: Vec<(String, &str, Value)> = vec![
        // Only the two split keys split; another list key is one lifted item.
        (
            "RUNHOLD_MAINTENANCE__PRUNE__NODES=a,b".to_owned(),
            "maintenance.prune.nodes",
            texts(&["a,b"]),
        ),
        (
            "RUNHOLD_TERMINAL__DEFAULT_SHELLS=/bin/bash, /bin/sh".to_owned(),
            "terminal.default_shells",
            texts(&["/bin/bash", " /bin/sh"]),
        ),
        (
            "RUNHOLD_TERMINAL__MAX_IDLE=0".to_owned(),
            "terminal.max_idle",
            Value::Duration(SignedDuration::ZERO),
        ),
        (
            "RUNHOLD_TERMINAL__MAX_IDLE=+0".to_owned(),
            "terminal.max_idle",
            Value::Duration(SignedDuration::ZERO),
        ),
        (
            "RUNHOLD_TERMINAL__MAX_IDLE=-0".to_owned(),
            "terminal.max_idle",
            Value::Duration(SignedDuration::ZERO),
        ),
        (
            "RUNHOLD_AUDIT__RETENTION=30d".to_owned(),
            "audit.retention",
            hours(720),
        ),
        (
            "RUNHOLD_AUDIT__RETENTION=1.5d".to_owned(),
            "audit.retention",
            hours(36),
        ),
        (
            "RUNHOLD_AUDIT__RETENTION=7d12h".to_owned(),
            "audit.retention",
            hours(180),
        ),
        (
            "RUNHOLD_AUDIT__RETENTION=-7d12h".to_owned(),
            "audit.retention",
            hours(-180),
        ),
        (
            "RUNHOLD_AUDIT__RETENTION=.5d".to_owned(),
            "audit.retention",
            hours(12),
        ),
        (
            "RUNHOLD_AUDIT__RETENTION=5.d".to_owned(),
            "audit.retention",
            hours(120),
        ),
        (
            "RUNHOLD_AUDIT__RETENTION=0d".to_owned(),
            "audit.retention",
            hours(0),
        ),
        // A variable without `__` is mapped to a one-segment key.
        ("RUNHOLD_DEBUG=1".to_owned(), "debug", Value::Bool(true)),
    ];
    // strconv.ParseBool.
    for (text, flag) in [
        ("1", true),
        ("t", true),
        ("T", true),
        ("TRUE", true),
        ("true", true),
        ("True", true),
        ("0", false),
        ("f", false),
        ("F", false),
        ("FALSE", false),
        ("false", false),
        ("False", false),
    ] {
        env_rows.push((
            format!("RUNHOLD_TERMINAL__ENABLED={text}"),
            "terminal.enabled",
            Value::Bool(flag),
        ));
    }
    // Decimal, as Go's base-0 parse reads it too.
    for (text, number) in [
        ("10", 10),
        ("+5", 5),
        ("-5", -5),
        ("0", 0),
        ("-0", 0),
        ("+0", 0),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
    ] {
        env_rows.push((
            format!("RUNHOLD_MAINTENANCE__DIGEST_REFRESH__CONCURRENCY={text}"),
            "maintenance.digest_refresh.concurrency",
            Value::Int(number),
        ));
    }
    for (entry, key, want) in &env_rows {
        let loaded = load("", &[entry.as_str()]);
        assert_eq!(loaded.problems, [], "{entry}");
        assert_eq!(loaded.value(key), Some(want), "{entry}");
    }
}

/// Every form difference 14 refuses, from the file and from the environment.
#[test]
#[expect(clippy::too_many_lines, reason = "one row per form")]
fn difference_14_refused_forms() {
    let file_rows: &[(&str, &str, &str)] = &[
        // A quoted number or boolean.
        (
            "maintenance:\n  digest_refresh:\n    concurrency: \"32\"\n",
            "maintenance.digest_refresh.concurrency",
            "expected an integer, got text",
        ),
        (
            "terminal:\n  enabled: \"true\"\n",
            "terminal.enabled",
            "expected true or false, got text",
        ),
        // An integer duration (Go: nanoseconds), but for a bare zero.
        (
            "terminal:\n  max_idle: 5\n",
            "terminal.max_idle",
            "expected a duration as text, got a number",
        ),
        // A boolean or a fraction where text is expected.
        (
            "server:\n  listen: true\n",
            "server.listen",
            "expected text, got a boolean",
        ),
        (
            "server:\n  listen: 1.5\n",
            "server.listen",
            "expected text, got a number",
        ),
        // A number for a boolean and a boolean for a number.
        (
            "terminal:\n  enabled: 1\n",
            "terminal.enabled",
            "expected true or false, got a number",
        ),
        (
            "maintenance:\n  digest_refresh:\n    concurrency: true\n",
            "maintenance.digest_refresh.concurrency",
            "expected an integer, got a boolean",
        ),
        // An empty string for either.
        (
            "terminal:\n  enabled: \"\"\n",
            "terminal.enabled",
            "expected true or false, got text",
        ),
        (
            "maintenance:\n  digest_refresh:\n    concurrency: \"\"\n",
            "maintenance.digest_refresh.concurrency",
            "expected an integer, got text",
        ),
        // A fraction for an integer (Go truncated).
        (
            "maintenance:\n  digest_refresh:\n    concurrency: 1.5\n",
            "maintenance.digest_refresh.concurrency",
            "invalid number: 1.5 is not a whole number",
        ),
        // A non-finite number where a list is expected is the wrong kind first (BitGauge's list
        // read checks no number), and a lifted one is refused by its item read.
        (
            "maintenance:\n  prune:\n    nodes: [.nan]\n",
            "maintenance.prune.nodes[0]",
            "invalid number: NaN is not a finite number",
        ),
        (
            "maintenance:\n  prune:\n    nodes: .nan\n",
            "maintenance.prune.nodes[0]",
            "invalid number: NaN is not a finite number",
        ),
        // A map is never lifted into a list.
        (
            "maintenance:\n  prune:\n    nodes: {a: b}\n",
            "maintenance.prune.nodes",
            "expected a list, got a map",
        ),
    ];
    for (text, key, message) in file_rows {
        let loaded = load(text, &[]);
        assert_eq!(
            loaded.problem_paths(),
            [*key],
            "{text:?}: {:?}",
            loaded.problems
        );
        assert_eq!(loaded.problems[0].message, *message, "{text:?}");
    }

    let mut env_rows: Vec<(String, &str)> = vec![
        ("RUNHOLD_TERMINAL__ENABLED=".to_owned(), "terminal.enabled"),
        (
            "RUNHOLD_MAINTENANCE__DIGEST_REFRESH__CONCURRENCY=".to_owned(),
            "maintenance.digest_refresh.concurrency",
        ),
        (
            "RUNHOLD_TERMINAL__MAX_IDLE=5".to_owned(),
            "terminal.max_idle",
        ),
        // A unit written twice: Go adds them (48h), jiff's grammar (difference 3) refuses it, as
        // it refuses `1h1h`.
        (
            "RUNHOLD_TERMINAL__MAX_IDLE=1d1d".to_owned(),
            "terminal.max_idle",
        ),
    ];
    for text in ["tRuE", "yes", "on", " true"] {
        env_rows.push((
            format!("RUNHOLD_TERMINAL__ENABLED={text}"),
            "terminal.enabled",
        ));
    }
    // Go's other bases (strconv.ParseInt(s, 0, 64) read them), and every malformed form.
    for text in [
        "0x10",
        "0X1f",
        "010",
        "-010",
        "00",
        "0o17",
        "0b101",
        "1_000",
        "0x_1F",
        "0_7",
        "_1",
        "1_",
        "1__0",
        "0x",
        "0b",
        "08",
        "-",
        "9223372036854775808",
        "-9223372036854775809",
        " 5",
        "5 ",
        "1e3",
        "0x1p4",
    ] {
        env_rows.push((
            format!("RUNHOLD_MAINTENANCE__DIGEST_REFRESH__CONCURRENCY={text}"),
            "maintenance.digest_refresh.concurrency",
        ));
    }
    for (entry, key) in &env_rows {
        let loaded = load("", &[entry.as_str()]);
        assert_eq!(
            loaded.problem_paths(),
            [*key],
            "{entry}: {:?}",
            loaded.problems
        );
    }
}

/// Go's koanf with yaml.v3 reads `true` in another case as text, as a label value and as a label
/// key, so a label Go accepted still types as text.
#[test]
fn mixed_case_booleans_in_the_file_are_label_text() {
    let loaded = load(
        "maintenance:\n  digest_refresh:\n    include_labels:\n      tRuE: FaLsE\n      tier: tRuE\n",
        &[],
    );
    assert_eq!(loaded.problems, []);
    assert_eq!(
        loaded.value("maintenance.digest_refresh.include_labels"),
        Some(&Value::TextMap(
            [("tRuE", "FaLsE"), ("tier", "tRuE")]
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect()
        ))
    );
    let loaded = load("terminal:\n  enabled: TRUE\n", &[]);
    assert_eq!(loaded.problems, []);
    assert_eq!(loaded.value("terminal.enabled"), Some(&Value::Bool(true)));
}

/// The segment after a map path keeps its case; deeper segments are lowercased again.
#[test]
fn map_paths_keep_their_key_case() {
    let loaded = load(
        "",
        &[
            "RUNHOLD_AUTH__REVERSE_PROXY__ROLE_MAP__Alice=admin",
            "RUNHOLD_MAINTENANCE__DIGEST_REFRESH__INCLUDE_LABELS__Com_Example_Tier=gold",
        ],
    );
    assert_eq!(loaded.problems, []);
    let map = |pairs: &[(&str, &str)]| {
        Value::TextMap(
            pairs
                .iter()
                .map(|&(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        )
    };
    assert_eq!(
        loaded.value("auth.reverse_proxy.role_map"),
        Some(&map(&[("Alice", "admin")]))
    );
    assert_eq!(
        loaded.value("maintenance.digest_refresh.include_labels"),
        Some(&map(&[("Com_Example_Tier", "gold")]))
    );

    let overlay = overlay(|path| path.first() == Some(&"compose_projects")).overlay([(
        "RUNHOLD_COMPOSE_PROJECTS__MediaStack__PROJECT_NAME",
        "media",
    )]);
    assert_eq!(overlay.unknown, []);
    let project = lookup(
        &overlay.tree,
        &["compose_projects", "MediaStack", "project_name"],
    );
    assert_eq!(project.value, Raw::Text("media".to_owned()));
    assert_eq!(
        project.origin,
        Origin::Environment("RUNHOLD_COMPOSE_PROJECTS__MediaStack__PROJECT_NAME".to_owned())
    );
}

#[expect(
    clippy::expect_used,
    reason = "a test helper: a broken fixture fails the test"
)]
fn lookup<'tree>(tree: &'tree Map, path: &[&str]) -> &'tree Node {
    let (last, parents) = path.split_last().expect("a path");
    let mut map = tree;
    for parent in parents {
        let Raw::Map(nested) = &map[*parent].value else {
            panic!("{parent} is a mapping");
        };
        map = nested;
    }
    &map[*last]
}

/// An unknown variable is returned with its path and value for the caller's exemptions.
#[test]
fn unknown_variables_are_returned_as_data() {
    let overlay = overlay(|path| SCHEMA.knows(path)).overlay([
        ("RUNHOLD_REGISTRY_PASSWORD", "from-a-password_env-key"),
        ("RUNHOLD_DEBUG_ENDPOINTS", "1"),
        ("RUNHOLD_SERVER__LISTN", ":1"),
    ]);
    let found: Vec<(String, &str, &str)> = overlay
        .unknown
        .iter()
        .map(|var| (var.dotted(), var.name.as_str(), var.value.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            ("debug_endpoints".to_owned(), "RUNHOLD_DEBUG_ENDPOINTS", "1"),
            (
                "registry_password".to_owned(),
                "RUNHOLD_REGISTRY_PASSWORD",
                "from-a-password_env-key"
            ),
            ("server.listn".to_owned(), "RUNHOLD_SERVER__LISTN", ":1"),
        ]
    );
    assert!(
        overlay.ignored.is_empty(),
        "nothing is ignored when unseparated names map"
    );
    assert!(overlay.tree.is_empty());
    let shown = format!("{:?}", overlay.unknown);
    assert!(!shown.contains("from-a-password_env-key"), "{shown}");
}

/// The empty value and the comma-only value under both profiles: bitgauge's (its test
/// `precedence`: an empty list variable clears the list; items trimmed) and Go's
/// (`strings.Split("", ",")` is `[""]`, `strings.Split(",", ",")` is `["", ""]`).
#[test]
fn empty_and_comma_only_list_values() {
    for (split, value, want) in [
        (BITGAUGE_SPLIT, "", vec![]),
        (BITGAUGE_SPLIT, ",", vec!["", ""]),
        (BITGAUGE_SPLIT, " a ,b ", vec!["a", "b"]),
        (GO_SPLIT, "", vec![""]),
        (GO_SPLIT, ",", vec!["", ""]),
        (GO_SPLIT, " a ,b ", vec![" a ", "b "]),
    ] {
        let overlay = EnvOverlay::new("RUNHOLD_", "__", Unseparated::Mapped, |_| true)
            .lists(|_| true, split)
            .overlay([("RUNHOLD_SERVER__TRUSTED_PROXIES", value)]);
        let node = lookup(&overlay.tree, &["server", "trusted_proxies"]);
        let Raw::List(items) = &node.value else {
            panic!("a list: {node:?}");
        };
        let got: Vec<String> = items.iter().map(|item| item.value.to_string()).collect();
        assert_eq!(got, want, "{split:?} {value:?}");
    }
}
