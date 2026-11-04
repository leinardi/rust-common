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

//! The logger's unit tests: the Go handlers' tests, ported, and the cases where the two plain
//! styles part.
//!
//! Extracted from swarm-device-access/src/logger/tests.rs at 8b4ab3a3e417 (the raw plain style,
//! `LogHandle`, the fallback parsers, the nil `[]string` and the stringer) and
//! swarm-scheduler-exporter/src/logger/tests.rs at c9e79afa8810 (the quoted plain style's escaping
//! cases, the strict parsers); the tests both copies share run under both plain styles.

use go_compat::quote;
use jiff::Timestamp;

use super::plain_handler::{level_to_upper, qualify};
use super::*;

/// Both plain styles, for the Go tests both handlers pass alike.
const STYLES: [PlainStyle; 2] = [PlainStyle::Quoted, PlainStyle::Raw];

/// A logger writing to a memory sink, with a fixed record time of 2025-01-15T10:00:00Z.
fn styled_logger(
    format: Format,
    level: Level,
    include_time: bool,
    plain_style: PlainStyle,
) -> (Logger, MemorySink) {
    let sink = MemorySink::default();
    let logger = Logger::new(
        Config {
            format,
            level,
            include_time,
            plain_style,
        },
        Box::new(sink.clone()),
        TimeZone::UTC,
        Arc::new(|| Timestamp::new(1_736_935_200, 0).unwrap()),
    );
    (logger, sink)
}

/// [`styled_logger`] for the text and JSON formats, which ignore the plain style.
fn logger(format: Format, level: Level, include_time: bool) -> (Logger, MemorySink) {
    styled_logger(format, level, include_time, PlainStyle::Raw)
}

/// Go's `newTestHandler`: plain, no time.
fn plain(style: PlainStyle, level: Level) -> (Logger, MemorySink) {
    styled_logger(Format::Plain, level, false, style)
}

/// Go: `TestParseLevel`, plus the U+0130 case of Go's `strings.ToLower`.
#[test]
fn parse_level_reads_the_names_in_any_case_and_defaults_to_info() {
    let cases = [
        ("debug", Level::DEBUG),
        ("DEBUG", Level::DEBUG),
        ("info", Level::INFO),
        ("INFO", Level::INFO),
        ("warn", Level::WARN),
        ("WARN", Level::WARN),
        ("warning", Level::WARN),
        ("error", Level::ERROR),
        ("fatal", Level::ERROR),
        ("panic", Level::ERROR),
        ("", Level::INFO),
        ("unknown", Level::INFO),
        ("VERBOSE", Level::INFO),
        // Go's strings.ToLower maps U+0130 to a plain i.
        ("\u{130}NFO", Level::INFO),
        ("WARN\u{130}NG", Level::WARN),
    ];
    for (input, want) in cases {
        assert_eq!(parse_level(input), want, "parse_level({input:?})");
    }
}

/// The format switch of Go's `newHandler`: json and plain in any case, text for anything else.
#[test]
fn parse_format_reads_the_names_in_any_case_and_defaults_to_text() {
    for (input, want) in [
        ("json", Format::Json),
        ("JSON", Format::Json),
        ("text", Format::Text),
        ("Text", Format::Text),
        ("plain", Format::Plain),
        ("PLAIN", Format::Plain),
        ("", Format::Text),
        ("logfmt", Format::Text),
    ] {
        assert_eq!(parse_format(input), want, "parse_format({input:?})");
    }
}

/// Go: `TestParseLevel`, with refusals in place of Go's silent fallback to info.
#[test]
fn parse_level_strict_accepts_the_names_in_any_case() {
    let cases = [
        ("debug", Level::DEBUG),
        ("DEBUG", Level::DEBUG),
        ("info", Level::INFO),
        ("INFO", Level::INFO),
        ("warn", Level::WARN),
        ("WARN", Level::WARN),
        ("warning", Level::WARN),
        ("error", Level::ERROR),
        ("fatal", Level::ERROR),
        ("panic", Level::ERROR),
        // Go's strings.ToLower maps U+0130 to a plain i.
        ("\u{130}NFO", Level::INFO),
        ("WARN\u{130}NG", Level::WARN),
    ];
    for (input, want) in cases {
        assert_eq!(
            parse_level_strict(input),
            Ok(want),
            "parse_level_strict({input:?})"
        );
    }
    for input in ["", "unknown", "VERBOSE", "trace", "info+1", " info"] {
        let error = parse_level_strict(input).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "invalid log level {input:?} (want one of debug, info, warn, warning, error, \
                 fatal, panic)"
            )
        );
    }
}

/// The format names, in any case, and nothing else.
#[test]
fn parse_format_strict_accepts_the_names_in_any_case() {
    for (input, want) in [
        ("json", Format::Json),
        ("JSON", Format::Json),
        ("text", Format::Text),
        ("Text", Format::Text),
        ("plain", Format::Plain),
        ("PLAIN", Format::Plain),
    ] {
        assert_eq!(
            parse_format_strict(input),
            Ok(want),
            "parse_format_strict({input:?})"
        );
    }
    for input in ["", "logfmt", "yaml"] {
        assert_eq!(
            parse_format_strict(input).unwrap_err().to_string(),
            format!("invalid log format {input:?} (want one of json, text, plain)")
        );
    }
}

/// The parsers lower a name with Go's `strings.ToLower`. A lowering on Rust's
/// `char::to_lowercase` with U+0130 mapped to a plain `i` (an earlier port's) reads every name the
/// same way: outside ASCII, the only characters either lowers into ASCII are U+0130 (to `i` in
/// both) and the Kelvin sign U+212A (to `k` in both), so the two accept the same spellings.
#[test]
fn only_two_non_ascii_characters_lower_to_ascii() {
    let go: Vec<char> = ('\u{80}'..=char::MAX)
        .filter(|&character| strings::to_lower_char(character).is_ascii())
        .collect();
    assert_eq!(go, ['\u{130}', '\u{212a}']);
    let rust: Vec<char> = ('\u{80}'..=char::MAX)
        .filter(|&character| character.to_lowercase().all(|lower| lower.is_ascii()))
        .collect();
    assert_eq!(rust, ['\u{212a}']);
    assert_eq!(strings::to_lower("\u{212a}\u{130}"), "ki");
    assert!('\u{212a}'.to_lowercase().eq(['k']));
}

/// Go: `TestTimeStripper_IncludeTime_ReturnsNil`: with the time on, nothing is stripped, a
/// user attribute keyed `time` included.
#[test]
fn include_time_keeps_every_time() {
    let (text, sink) = logger(Format::Text, Level::INFO, true);
    text.info("m", &[Attr::string("time", "x")]);
    assert_eq!(
        sink.contents(),
        "time=2025-01-15T10:00:00.000Z level=INFO msg=m time=x\n"
    );
}

/// Go: `TestTimeStripper_ExcludeTime_DropsTimeKey`: the record time and every attribute keyed
/// `time` are dropped, groups included, with slog's quirks when that empties a group (the text
/// prefix stays, and JSON loses a comma). Expected lines are Go's output.
#[test]
fn exclude_time_drops_every_time_key() {
    let cases: [(&str, Vec<Attr>, &str, &str); 4] = [
        (
            "top",
            vec![Attr::string("time", "x"), Attr::int("k", 1)],
            "level=INFO msg=top k=1",
            r#"{"level":"INFO","msg":"top","k":1}"#,
        ),
        (
            "group",
            vec![
                Attr::group("g", vec![Attr::string("time", "x")]),
                Attr::int("k", 1),
            ],
            "level=INFO msg=group g.k=1",
            r#"{"level":"INFO","msg":"group""k":1}"#,
        ),
        (
            "only",
            vec![Attr::string("time", "x")],
            "level=INFO msg=only",
            r#"{"level":"INFO","msg":"only"}"#,
        ),
        (
            "nested",
            vec![Attr::group(
                "g",
                vec![Attr::string("time", "x"), Attr::int("a", 2)],
            )],
            "level=INFO msg=nested g.a=2",
            r#"{"level":"INFO","msg":"nested","g":{"a":2}}"#,
        ),
    ];
    for (message, attrs, want_text, want_json) in cases {
        let (text, text_sink) = logger(Format::Text, Level::INFO, false);
        text.info(message, &attrs);
        assert_eq!(
            text_sink.contents(),
            format!("{want_text}\n"),
            "text {message}"
        );
        let (json, json_sink) = logger(Format::Json, Level::INFO, false);
        json.info(message, &attrs);
        assert_eq!(
            json_sink.contents(),
            format!("{want_json}\n"),
            "json {message}"
        );
    }

    let (text, sink) = logger(Format::Text, Level::INFO, false);
    text.with_attrs(&[Attr::string("time", "w"), Attr::string("c", "d")])
        .info("with", &[]);
    text.with_group("wg")
        .info("withgroup", &[Attr::string("time", "x")]);
    text.with_group("wg").info(
        "withgroup2",
        &[Attr::string("time", "x"), Attr::int("k", 1)],
    );
    assert_eq!(
        sink.contents(),
        "level=INFO msg=with c=d\nlevel=INFO msg=withgroup\nlevel=INFO msg=withgroup2 wg.k=1\n"
    );
}

/// Go: `TestTimeStripper_ExcludeTime_PassesOtherAttrs`.
#[test]
fn exclude_time_passes_other_attrs() {
    let (text, sink) = logger(Format::Text, Level::INFO, false);
    text.info("m", &[Attr::string("service", "myapp")]);
    assert_eq!(sink.contents(), "level=INFO msg=m service=myapp\n");
}

/// Go: `TestPlainHandler_BasicLine`
#[test]
fn plain_basic_line() {
    for style in STYLES {
        let (logger, sink) = plain(style, Level::INFO);
        logger.info("hello world", &[]);
        assert_eq!(sink.contents(), "level=INFO hello world\n", "{style:?}");
    }
}

/// Go: `TestPlainHandler_WithAttr`
#[test]
fn plain_with_attr() {
    for style in STYLES {
        let (logger, sink) = plain(style, Level::INFO);
        logger.info("msg", &[Attr::string("k", "v")]);
        assert_eq!(sink.contents(), "level=INFO msg k=v\n", "{style:?}");
    }
}

/// Go: `TestPlainHandler_QuotedValue`
#[test]
fn plain_quoted_value() {
    for style in STYLES {
        let (logger, sink) = plain(style, Level::INFO);
        logger.info("msg", &[Attr::string("k", "hello world")]);
        assert_eq!(
            sink.contents(),
            "level=INFO msg k=\"hello world\"\n",
            "{style:?}"
        );
    }
}

/// Go: `TestPlainHandler_LevelVariants`
#[test]
fn plain_level_variants() {
    for style in STYLES {
        for (level, want) in [
            (Level::DEBUG, "DEBUG"),
            (Level::INFO, "INFO"),
            (Level::WARN, "WARN"),
            (Level::ERROR, "ERROR"),
        ] {
            let (logger, sink) = plain(style, level);
            logger.log(level, "", &[]);
            assert_eq!(
                sink.contents(),
                format!("level={want}\n"),
                "{style:?} {level}"
            );
        }
    }
}

/// Go: `TestPlainHandler_Enabled`
#[test]
fn plain_enabled() {
    for style in STYLES {
        let (logger, _) = plain(style, Level::WARN);
        assert!(!logger.enabled(Level::INFO));
        assert!(logger.enabled(Level::WARN));
        assert!(logger.enabled(Level::ERROR));
    }
}

/// Go: `TestPlainHandler_WithGroup`
#[test]
fn plain_with_group() {
    for style in STYLES {
        let (logger, sink) = plain(style, Level::INFO);
        logger
            .with_group("g1")
            .info("msg", &[Attr::string("k", "v")]);
        assert_eq!(sink.contents(), "level=INFO msg g1.k=v\n", "{style:?}");
    }
}

/// Go: `TestPlainHandler_WithAttrs`
#[test]
fn plain_with_attrs() {
    for style in STYLES {
        let (logger, sink) = plain(style, Level::INFO);
        logger
            .with_attrs(&[Attr::string("prekey", "preval")])
            .info("msg", &[Attr::string("k", "v")]);
        assert_eq!(
            sink.contents(),
            "level=INFO msg prekey=preval k=v\n",
            "{style:?}"
        );
    }
}

/// Go: `TestPlainHandler_WithAttrs_Empty_ReturnsSelf`
#[test]
fn plain_with_no_attrs_returns_the_same_logger() {
    for style in STYLES {
        let (logger, _) = plain(style, Level::INFO);
        let derived = logger.with_attrs(&[]);
        assert!(Arc::ptr_eq(&logger.handler, &derived.handler), "{style:?}");
    }
}

/// Go: `TestPlainHandler_WithGroup_EmptyName_ReturnsSelf`: a blank name is trimmed to nothing
/// and changes nothing.
#[test]
fn plain_with_blank_group_changes_nothing() {
    for style in STYLES {
        let (logger, sink) = plain(style, Level::INFO);
        let derived = logger.with_group("   ");
        assert_eq!(derived.handler, logger.handler);
        derived.info("msg", &[Attr::string("k", "v")]);
        assert_eq!(sink.contents(), "level=INFO msg k=v\n", "{style:?}");
        assert!(Arc::ptr_eq(&logger.with_group("").handler, &logger.handler));
    }
}

/// Go: `TestPlainHandler_OriginalUnmutatedByWithAttrs`
#[test]
fn plain_original_unchanged_by_with_attrs() {
    for style in STYLES {
        let (original, sink) = plain(style, Level::INFO);
        let derived = original.with_attrs(&[Attr::string("extra", "x")]);
        original.info("msg", &[]);
        derived.info("msg", &[]);
        assert_eq!(
            sink.contents(),
            "level=INFO msg\nlevel=INFO msg extra=x\n",
            "{style:?}"
        );
    }
}

/// Go: `TestPlainHandler_GroupAttr`
#[test]
fn plain_group_attr() {
    for style in STYLES {
        let (logger, sink) = plain(style, Level::INFO);
        logger.info("msg", &[Attr::group("g", vec![Attr::int("n", 42)])]);
        assert_eq!(sink.contents(), "level=INFO msg g={g.n=42}\n", "{style:?}");
    }
}

/// Go: `TestPlainHandler_IncludeTime`
#[test]
fn plain_include_time() {
    for style in STYLES {
        let (logger, sink) = styled_logger(Format::Plain, Level::INFO, true, style);
        logger.info("msg", &[]);
        assert_eq!(
            sink.contents(),
            "time=2025-01-15T10:00:00Z level=INFO msg\n",
            "{style:?}"
        );
    }
}

/// Go: `TestLevelToUpper`
#[test]
fn plain_level_to_upper() {
    for (level, want) in [
        (Level::DEBUG, "DEBUG"),
        (Level(-5), "DEBUG"),
        (Level::INFO, "INFO"),
        (Level::WARN, "WARN"),
        (Level::ERROR, "ERROR"),
        (Level(9), "ERROR"),
        // Not exactly info or warn: the plain format reads ERROR.
        (Level(1), "ERROR"),
    ] {
        assert_eq!(level_to_upper(level), want, "{level}");
    }
}

/// Go: `TestQualify_NoGroups`
#[test]
fn plain_qualify_without_groups() {
    assert_eq!(qualify(&[], &Attr::string("key", "val")).key, "key");
}

/// Go: `TestQualify_WithGroups`
#[test]
fn plain_qualify_with_groups() {
    let groups = ["g1".to_owned(), "g2".to_owned()];
    assert_eq!(qualify(&groups, &Attr::string("k", "v")).key, "g1.g2.k");
}

/// [`PlainStyle::Raw`] where [`PlainStyle::Quoted`] escapes: the message and keys raw, a newline
/// included; a string quoted only for a space or a tab, with only `"` escaped; an error, a
/// stringer and a `[]string` raw; an attribute without a key dropped; and an empty group from
/// `with_attrs` written `key={}`. Expected lines are Go's output.
#[test]
fn plain_raw_keeps_the_go_handler_rules() {
    let (logger, sink) = plain(PlainStyle::Raw, Level::INFO);
    logger.info("two\nlines", &[Attr::string("a key", "x")]);
    logger.info(
        "msg",
        &[
            Attr::string("tab", "a\tb"),
            Attr::string("quote", r#"say "hi""#),
            Attr::string("bare", r#"a"b\c"#),
            Attr::string("empty", ""),
            Attr::error("err", &"dial tcp: connection refused"),
            Attr::stringer("device", &"c 226:0"),
            Attr::strings("list", ["a b", "c"]),
            Attr::nil_strings("none"),
            Attr::string("", "dropped"),
            Attr::group("", vec![Attr::string("inlined", "dropped")]),
        ],
    );
    logger
        .with_attrs(&[Attr::group("g", vec![])])
        .info("msg", &[]);
    assert_eq!(
        sink.contents(),
        "level=INFO two\nlines a key=x\n\
         level=INFO msg tab=\"a\tb\" quote=\"say \\\"hi\\\"\" bare=a\"b\\c empty= \
         err=dial tcp: connection refused device=c 226:0 list=[a b c] none=[]\n\
         level=INFO msg g={}\n"
    );
}

/// The same records under [`PlainStyle::Quoted`]: one line each, the message escaped, a key or a
/// text value quoted unless plainly safe (a stringer and a nil `[]string` as the other text
/// values), an attribute without a key written `""=` unless it is a group, and an empty group
/// from `with_attrs` written `key={}`.
#[test]
fn plain_quoted_escapes_what_raw_writes_as_is() {
    let (logger, sink) = plain(PlainStyle::Quoted, Level::INFO);
    logger.info("two\nlines", &[Attr::string("a key", "x")]);
    logger.info(
        "msg",
        &[
            Attr::string("tab", "a\tb"),
            Attr::string("quote", r#"say "hi""#),
            Attr::string("bare", r#"a"b\c"#),
            Attr::string("empty", ""),
            Attr::error("err", &"dial tcp: connection refused"),
            Attr::stringer("device", &"c 226:0"),
            Attr::stringer("bare_device", &"b"),
            Attr::strings("list", ["a b", "c"]),
            Attr::nil_strings("none"),
            Attr::string("", "kept"),
            Attr::group("", vec![Attr::string("inlined", "dropped")]),
        ],
    );
    logger
        .with_attrs(&[Attr::group("g", vec![])])
        .info("msg", &[]);
    assert_eq!(
        sink.contents(),
        "level=INFO two\\nlines \"a key\"=x\n\
         level=INFO msg tab=\"a\\tb\" quote=\"say \\\"hi\\\"\" bare=\"a\\\"b\\\\c\" empty=\"\" \
         err=\"dial tcp: connection refused\" device=\"c 226:0\" bare_device=b \
         list=\"[a b c]\" none=[] \"\"=kept\n\
         level=INFO msg g={}\n"
    );
}

/// Where an escaping case puts its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    Message,
    TopKey,
    GroupName,
    GroupChildKey,
    StringValue,
    ErrorValue,
    GroupValue,
}

const TEXT_POSITIONS: [Position; 6] = [
    Position::Message,
    Position::TopKey,
    Position::GroupChildKey,
    Position::StringValue,
    Position::ErrorValue,
    Position::GroupValue,
];
/// Every position; the group name is trimmed by `with_group`.
const ALL_POSITIONS: [Position; 7] = [
    Position::Message,
    Position::TopKey,
    Position::GroupChildKey,
    Position::StringValue,
    Position::ErrorValue,
    Position::GroupValue,
    Position::GroupName,
];
const KEY_POSITIONS: [Position; 3] = [
    Position::TopKey,
    Position::GroupName,
    Position::GroupChildKey,
];
const VALUE_POSITIONS: [Position; 3] = [
    Position::StringValue,
    Position::ErrorValue,
    Position::GroupValue,
];

/// Logs `text` at `position` and returns the line.
fn log_at(position: Position, text: &str) -> String {
    let (base, sink) = plain(PlainStyle::Quoted, Level::INFO);
    let mut logger = base.clone();
    let mut message = "msg";
    let attrs = match position {
        Position::Message => {
            message = text;
            vec![Attr::int("after", 1)]
        }
        Position::TopKey => vec![Attr::string(text, "v")],
        Position::GroupName => {
            logger = base.with_group(text);
            vec![Attr::string("k", "v")]
        }
        Position::GroupChildKey => vec![Attr::group("g", vec![Attr::string(text, "v")])],
        Position::StringValue => vec![Attr::string("k", text)],
        Position::ErrorValue => vec![Attr::error("err", &text)],
        Position::GroupValue => vec![Attr::group("g", vec![Attr::string("k", text)])],
    };
    logger.info(message, &attrs);
    sink.contents()
}

/// The exact line `log_at(position, text)` must produce, from the escaped message and the quoted
/// form of the text.
fn want_at(position: Position, message: &str, quoted: &str) -> String {
    let inner = &quoted[1..quoted.len() - 1];
    match position {
        Position::Message => format!("level=INFO {message} after=1\n"),
        Position::TopKey => format!("level=INFO msg {quoted}=v\n"),
        Position::GroupName => format!("level=INFO msg \"{inner}.k\"=v\n"),
        Position::GroupChildKey => format!("level=INFO msg g={{\"g.{inner}\"=v}}\n"),
        Position::StringValue => format!("level=INFO msg k={quoted}\n"),
        Position::ErrorValue => format!("level=INFO msg err={quoted}\n"),
        Position::GroupValue => format!("level=INFO msg g={{g.k={quoted}}}\n"),
    }
}

/// The escaping cases: a name, the text, its escaped form in the message (empty where the
/// case puts nothing there) and its quoted form, and the positions it is logged at.
type EscapingCase = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [Position],
);

const ESCAPING_CASES: [EscapingCase; 20] = [
    (
        "forged record",
        "\nlevel=ERROR forged",
        r"\nlevel=ERROR forged",
        r#""\nlevel=ERROR forged""#,
        // with_group trims the leading newline of a group name.
        &TEXT_POSITIONS,
    ),
    (
        "ESC",
        "\x1b[31m",
        r"\x1b[31m",
        r#""\x1b[31m""#,
        &ALL_POSITIONS,
    ),
    (
        "U+0085",
        "a\u{85}b",
        r"a\u0085b",
        r#""a\u0085b""#,
        &ALL_POSITIONS,
    ),
    (
        "U+009B",
        "a\u{9b}b",
        r"a\u009bb",
        r#""a\u009bb""#,
        &ALL_POSITIONS,
    ),
    (
        "U+2028",
        "a\u{2028}b",
        r"a\u2028b",
        r#""a\u2028b""#,
        &ALL_POSITIONS,
    ),
    (
        "U+2029",
        "a\u{2029}b",
        r"a\u2029b",
        r#""a\u2029b""#,
        &ALL_POSITIONS,
    ),
    (
        "bidi override",
        "a\u{202e}b",
        r"a\u202eb",
        r#""a\u202eb""#,
        &ALL_POSITIONS,
    ),
    (
        "quote without a space",
        r#"a"b"#,
        r#"a"b"#,
        r#""a\"b""#,
        &ALL_POSITIONS,
    ),
    (
        "newline without a space",
        "a\nb",
        r"a\nb",
        r#""a\nb""#,
        &ALL_POSITIONS,
    ),
    ("backslash", r"a\b", r"a\\b", r#""a\\b""#, &ALL_POSITIONS),
    (
        "error with spaces",
        "dial tcp: connection refused",
        "",
        r#""dial tcp: connection refused""#,
        &[Position::ErrorValue],
    ),
    ("empty value", "", "", r#""""#, &VALUE_POSITIONS),
    ("empty key", "", "", r#""""#, &[Position::TopKey]),
    (
        "key with a space and =",
        "x level=ERROR",
        "",
        r#""x level=ERROR""#,
        &KEY_POSITIONS,
    ),
    (
        "key with a quote",
        r#"a"b"#,
        "",
        r#""a\"b""#,
        &KEY_POSITIONS,
    ),
    (
        "key with a close brace",
        "a}b",
        "",
        r#""a}b""#,
        &KEY_POSITIONS,
    ),
    (
        "key with an open brace",
        "a{b",
        "",
        r#""a{b""#,
        &KEY_POSITIONS,
    ),
    (
        "group name",
        "g h=1",
        "",
        r#""g h=1""#,
        &[Position::GroupName],
    ),
    ("close brace value", "}", "", r#""}""#, &VALUE_POSITIONS),
    ("open brace value", "{", "", r#""{""#, &VALUE_POSITIONS),
];

/// Go: `TestPlainHandler_Escaping`, for [`PlainStyle::Quoted`]: hostile text in every position
/// of a plain line gives exactly one line, with exactly the escaped text. Go also parses each line
/// back into its fields; here the exact line is asserted, which pins the same fields. Go's
/// "invalid byte" case has no Rust counterpart: a `&str` cannot hold an invalid byte.
#[test]
fn plain_escaping() {
    for (name, text, message, quoted, positions) in ESCAPING_CASES {
        // The quoted form is strconv.Quote's.
        assert_eq!(quote::quote(text), quoted, "{name}: quoted form");
        for &position in positions {
            let got = log_at(position, text);
            assert_eq!(got.matches('\n').count(), 1, "{name}/{position:?}: {got:?}");
            assert_eq!(
                got,
                want_at(position, message, quoted),
                "{name}/{position:?}"
            );
        }
    }
}

fn handle(format: Format) -> (LogHandle, MemorySink) {
    let (logger, sink) = logger(format, Level::INFO, false);
    (LogHandle::new(logger), sink)
}

/// A reconfigure reaches every clone of the handle, and the handles made by `with_attrs`, from
/// their next record; the clock and zone carry over.
#[test]
fn configure_swaps_the_logger_for_every_clone() {
    let (handle, first) = handle(Format::Text);
    let clone = handle.clone();
    let component = handle.with_attrs(&[Attr::string("component", "launcher")]);
    clone.info("before", &[]);
    component.info("before", &[]);

    let second = MemorySink::default();
    handle.configure_to(
        Config::parse("plain", "warn", true, PlainStyle::Raw),
        Box::new(second.clone()),
    );
    clone.info("dropped below warn", &[]);
    clone.warn("after", &[]);
    component.warn("after", &[Attr::int("n", 1)]);

    assert_eq!(
        first.contents(),
        "level=INFO msg=before\nlevel=INFO msg=before component=launcher\n"
    );
    assert_eq!(
        second.contents(),
        "time=2025-01-15T10:00:00Z level=WARN after\n\
         time=2025-01-15T10:00:00Z level=WARN after component=launcher n=1\n"
    );
    assert!(!component.enabled(Level::INFO));
}

/// Go's `Set`: the logger a handle installs is the one every clone logs through.
#[test]
fn set_replaces_the_logger() {
    let (handle, first) = handle(Format::Text);
    let (replacement, second) = logger(Format::Json, Level::DEBUG, false);
    handle.clone().set(replacement);
    handle.debug("m", &[]);
    assert_eq!(first.contents(), "");
    assert_eq!(second.contents(), "{\"level\":\"DEBUG\",\"msg\":\"m\"}\n");
}

/// The two value kinds only one of the extracted copies had, in text and JSON: a nil `[]string`
/// (`[]` in text, `null` in JSON) and a string-typed stringer, which JSON writes through
/// encoding/json without HTML escaping. Expected lines are Go's output.
#[test]
fn nil_strings_and_stringer_match_slog() {
    let attrs = [Attr::nil_strings("none"), Attr::stringer("s", &"<a>&")];
    let (text, text_sink) = logger(Format::Text, Level::INFO, false);
    text.info("m", &attrs);
    assert_eq!(text_sink.contents(), "level=INFO msg=m none=[] s=<a>&\n");
    let (json, json_sink) = logger(Format::Json, Level::INFO, false);
    json.info(
        "m",
        &[
            Attr::nil_strings("none"),
            Attr::stringer("s", &"<a>&\u{2028}"),
        ],
    );
    assert_eq!(
        json_sink.contents(),
        "{\"level\":\"INFO\",\"msg\":\"m\",\"none\":null,\"s\":\"<a>&\\u2028\"}\n"
    );
}

/// `LogHandle::configure` keeps the plain style the handle was made with, as it keeps the clock
/// and the zone.
#[test]
fn configure_keeps_the_plain_style() {
    for style in STYLES {
        let (logger, _) = styled_logger(Format::Text, Level::INFO, false, style);
        let handle = LogHandle::new(logger);
        // To stdout, so nothing is logged through it.
        handle.configure("json", "error", false);
        assert_eq!(handle.logger().plain_style(), style);
        let sink = MemorySink::default();
        handle.configure_to(
            Config::parse("PLAIN", "bogus", false, handle.logger().plain_style()),
            Box::new(sink.clone()),
        );
        handle.info("a\nb", &[]);
        let want = match style {
            PlainStyle::Quoted => "level=INFO a\\nb\n",
            PlainStyle::Raw => "level=INFO a\nb\n",
        };
        assert_eq!(sink.contents(), want, "{style:?}");
    }
}

/// The process-wide logger is the one `set_global` installed, for every caller.
#[test]
fn set_global_replaces_the_process_wide_logger() {
    let (logger, sink) = styled_logger(Format::Plain, Level::INFO, false, PlainStyle::Quoted);
    set_global(logger);
    global().info("one", &[Attr::string("k", "v w")]);
    global().debug("dropped", &[]);
    assert_eq!(sink.contents(), "level=INFO one k=\"v w\"\n");
}
