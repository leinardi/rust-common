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

//! The negotiation and the answer, under both expfmt versions.
//!
//! Extracted from swarm-scheduler-exporter/src/metrics/exposition/tests.rs at c9e79afa8810 (the
//! v0.71.0 corpus) and swarm-device-access/src/metrics/exposition/tests.rs at 8b4ab3a3e417 (the
//! v0.70.1 corpus, the wildcard clauses).

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;

use flate2::read::GzDecoder;

use super::{
    Encoding, Escaping, ExpfmtVersion, Format, Negotiation, content_type, go_content_type,
    negotiate, negotiate_encoding, parse_accept, render,
};
use crate::{Desc, Gauge, Kind, Registry};

/// One row of a negotiation corpus: the `Accept` and `Accept-Encoding` header values (each list
/// `|`-separated, `-` for no header), and the `Content-Type` and `Content-Encoding` promhttp
/// answered.
struct GoAnswer<'a> {
    accept: Vec<&'a str>,
    accept_encoding: Vec<&'a str>,
    content_type: &'a str,
    content_encoding: &'a str,
}

fn header_values(field: &str) -> Vec<&str> {
    if field == "-" {
        Vec::new()
    } else {
        field.split('|').collect()
    }
}

fn go_answers(data: &str) -> Vec<GoAnswer<'_>> {
    data.lines()
        .map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            assert_eq!(columns.len(), 4, "malformed line {line:?}");
            GoAnswer {
                accept: header_values(columns[0]),
                accept_encoding: header_values(columns[1]),
                content_type: columns[2],
                content_encoding: columns[3],
            }
        })
        .collect()
}

/// A corpus under `tests/testdata/exposition`.
fn corpus(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata/exposition")
        .join(file);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The `Accept` header of every row of `answers` where `version`'s negotiation differs from Go's
/// answer, in Go's `Content-Type` for the negotiation itself, the text `Content-Type` with Go's
/// escaping, or the `Content-Encoding`; and how many rows Go answered with protobuf.
fn mismatches(version: ExpfmtVersion, answers: &[GoAnswer<'_>]) -> (Vec<String>, usize) {
    let mut differing = Vec::new();
    let mut protobuf = 0;
    for answer in answers {
        // Go reads the first Accept header only (Header.Get).
        let accept = answer.accept.first().copied();
        let (_, escaping) = answer
            .content_type
            .rsplit_once("; escaping=")
            .expect("Go always appends escaping");
        let want = format!("text/plain; version=0.0.4; charset=utf-8; escaping={escaping}");
        let want_encoding =
            (!answer.content_encoding.is_empty()).then_some(answer.content_encoding);
        if go_content_type(version, accept) != answer.content_type
            || content_type(version, accept) != want
            || negotiate_encoding(&answer.accept_encoding).header() != want_encoding
        {
            differing.push(format!("{:?} {:?}", answer.accept, answer.accept_encoding));
        }
        if !answer.content_type.starts_with("text/plain") {
            protobuf += 1;
        }
    }
    (differing, protobuf)
}

/// promhttp's answers, the Prometheus Go client v1.24.1 and common v0.71.0, for 95 header pairs
/// from a throwaway Go program serving a `promhttp.HandlerFor` handler
/// (swarm-scheduler-exporter's `go-negotiation.tsv`): the first 24 rows are its S2
/// `negotiation.tsv`, the rest probe the parsers (q values `ParseFloat` reads, more than 12
/// clauses, so goautoneg's sort leaves insertion sort, repeated headers, and the gzip rules). Each
/// row asserts Go's `Content-Type` for the negotiation itself, the text `Content-Type` with Go's
/// escaping where Go picks protobuf, and the `Content-Encoding`.
#[test]
fn v0_71_0_matches_go_on_95_header_pairs() {
    let data = corpus("negotiation-v0.71.0.tsv");
    let answers = go_answers(&data);
    assert_eq!(answers.len(), 95);
    let (differing, protobuf) = mismatches(ExpfmtVersion::V0_71_0, &answers);
    assert_eq!(differing, Vec::<String>::new());
    assert!(
        protobuf >= 10,
        "only {protobuf} rows where Go picks protobuf"
    );
}

/// What swarm-device-access's Go `/metrics` handler answers (the Prometheus Go client v1.24.1,
/// common v0.70.1) for 106 header pairs, captured by its `TestGoldenNegotiation`: the first 95 are
/// the v0.71.0 corpus's header pairs, the rest probe the wildcard types v0.70.1 passes over.
#[test]
fn v0_70_1_matches_go_on_106_header_pairs() {
    let data = corpus("negotiation-v0.70.1.tsv");
    let answers = go_answers(&data);
    assert_eq!(answers.len(), 106);
    let (differing, protobuf) = mismatches(ExpfmtVersion::V0_70_1, &answers);
    assert_eq!(differing, Vec::<String>::new());
    assert!(
        protobuf >= 10,
        "only {protobuf} rows where Go picks protobuf"
    );
}

/// The two releases answer the 95 header pairs of the v0.71.0 corpus alike: its rows are the first
/// 95 of the v0.70.1 corpus, byte for byte, and pass under either negotiation.
#[test]
fn the_v0_71_0_corpus_passes_under_both_versions() {
    let newer = corpus("negotiation-v0.71.0.tsv");
    let older = corpus("negotiation-v0.70.1.tsv");
    assert!(
        older.starts_with(&newer),
        "the v0.70.1 corpus does not start with the v0.71.0 one"
    );
    let answers = go_answers(&newer);
    for version in [ExpfmtVersion::V0_70_1, ExpfmtVersion::V0_71_0] {
        assert_eq!(
            mismatches(version, &answers).0,
            Vec::<String>::new(),
            "{version:?}"
        );
    }
}

/// The negative control: the v0.70.1 corpus's wildcard rows fail under v0.71.0, so the version
/// parameter is what tells the two Go builds apart.
#[test]
fn the_v0_70_1_wildcard_rows_need_their_version() {
    let data = corpus("negotiation-v0.70.1.tsv");
    let (differing, _) = mismatches(ExpfmtVersion::V0_71_0, &go_answers(&data));
    assert_eq!(differing, WILDCARD_ROWS);
}

/// The rows of the v0.70.1 corpus that v0.71.0 answers otherwise: a wildcard clause alone, which
/// v0.71.0 matches to a protobuf format.
const WILDCARD_ROWS: [&str; 2] = [
    r#"["application/*;proto=io.prometheus.client.MetricFamily;encoding=delimited"] []"#,
    r#"["*/*;proto=io.prometheus.client.MetricFamily;encoding=text"] []"#,
];

/// A wildcard clause: v0.70.1 picks no format on it, so a later clause's escaping counts, where
/// v0.71.0 stops at it. goautoneg sorts a concrete type or subtype ahead of a wildcard one, so with
/// a `text/plain` clause also given both versions stop at that one. The v0.70.1 answers are Go's
/// (the corpus's last rows); the v0.71.0 ones follow its `matchFormat`, which no Go capture here
/// exercises on a wildcard.
#[test]
fn wildcard_clauses_by_version() {
    let text = "text/plain; version=0.0.4; charset=utf-8";
    let delimited = "application/vnd.google.protobuf; proto=io.prometheus.client.MetricFamily; \
                     encoding=delimited";
    let proto_text =
        "application/vnd.google.protobuf; proto=io.prometheus.client.MetricFamily; encoding=text";
    for (accept, older, newer) in [
        (
            "application/*;proto=io.prometheus.client.MetricFamily;encoding=delimited",
            format!("{text}; escaping=underscores"),
            format!("{delimited}; escaping=underscores"),
        ),
        (
            "application/*;proto=io.prometheus.client.MetricFamily;encoding=delimited,\
             text/plain;escaping=dots",
            format!("{text}; escaping=dots"),
            format!("{text}; escaping=dots"),
        ),
        (
            "*/plain;version=0.0.4;escaping=values,text/plain;escaping=dots",
            format!("{text}; escaping=dots"),
            format!("{text}; escaping=dots"),
        ),
        (
            "*/*;proto=io.prometheus.client.MetricFamily;encoding=text",
            format!("{text}; escaping=underscores"),
            format!("{proto_text}; escaping=underscores"),
        ),
    ] {
        assert_eq!(
            go_content_type(ExpfmtVersion::V0_70_1, Some(accept)),
            older,
            "v0.70.1 {accept}"
        );
        assert_eq!(
            go_content_type(ExpfmtVersion::V0_71_0, Some(accept)),
            newer,
            "v0.71.0 {accept}"
        );
    }
}

/// [`negotiate`] gives the format and escaping the content types are built from.
#[test]
fn negotiate_names_the_format_and_escaping() {
    let accept = "application/vnd.google.protobuf;proto=io.prometheus.client.MetricFamily;\
                  encoding=compact-text;escaping=values";
    for version in [ExpfmtVersion::V0_70_1, ExpfmtVersion::V0_71_0] {
        assert_eq!(
            negotiate(version, Some(accept)),
            Negotiation {
                format: Format::ProtoCompact,
                escaping: Escaping::Values,
            }
        );
        assert_eq!(
            negotiate(version, None),
            Negotiation {
                format: Format::Text,
                escaping: Escaping::Underscores,
            }
        );
    }
    assert_eq!(Escaping::AllowUtf8.as_str(), "allow-utf-8");
    assert_eq!(Format::Text.content_type(), crate::text::TEXT_CONTENT_TYPE);
}

/// goautoneg's parse: spaces trimmed around the parts, `q` read untrimmed, a malformed clause
/// skipped, and the last of a repeated parameter kept.
#[test]
fn parse_accept_follows_goautoneg() {
    let clauses = parse_accept(" text/plain ; a=1;a=2 ; q=0.5,*,bad,x/y/z,, application/json;q= 1");
    let summary: Vec<(String, String, f64, String)> = clauses
        .iter()
        .map(|clause| {
            (
                clause.media_type.clone(),
                clause.subtype.clone(),
                clause.quality,
                clause.param("a").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("*".to_owned(), "*".to_owned(), 1.0, String::new()),
            ("text".to_owned(), "plain".to_owned(), 0.5, "2".to_owned()),
            (
                "application".to_owned(),
                "json".to_owned(),
                0.0,
                String::new()
            ),
        ]
    );
}

/// The gzip rules, against the Go answers' reasons: a tie goes to identity, the name is
/// case-sensitive, a best q of 0 is identity, and every header counts.
#[test]
fn negotiate_encoding_rules() {
    assert_eq!(negotiate_encoding(&[]), Encoding::Identity);
    assert_eq!(negotiate_encoding(&["gzip"]), Encoding::Gzip);
    assert_eq!(negotiate_encoding(&["gzip, identity"]), Encoding::Identity);
    assert_eq!(negotiate_encoding(&["GZIP"]), Encoding::Identity);
    assert_eq!(negotiate_encoding(&["gzip;q=0"]), Encoding::Identity);
    assert_eq!(
        negotiate_encoding(&["identity;q=0, gzip;q=0"]),
        Encoding::Identity
    );
    assert_eq!(negotiate_encoding(&["deflate", "gzip"]), Encoding::Gzip);
    // gddo reads q as 0 or 1 then decimals: 2 is not a quality, so the element is dropped.
    assert_eq!(negotiate_encoding(&["gzip;q=2"]), Encoding::Identity);
    assert_eq!(negotiate_encoding(&["gzip;q=1.5"]), Encoding::Gzip);
}

fn one_gauge() -> Registry {
    let registry = Registry::new();
    let gauge = Arc::new(Gauge::new(
        Desc::new("g", "h", Kind::Gauge, &[]).expect("desc"),
    ));
    gauge.set(1.0);
    registry.register(gauge).expect("register");
    let other = Arc::new(Gauge::new(
        Desc::new("o", "other", Kind::Gauge, &[]).expect("desc"),
    ));
    registry.register(other).expect("register");
    registry
}

const ONE_GAUGE: &str = "# HELP g h\n# TYPE g gauge\ng 1\n# HELP o other\n# TYPE o gauge\no 0\n";

/// The answer: the body as text, gzipped with Go's header bytes when gzip is negotiated (no
/// name, mtime 0, the default level's XFL 0 and OS 255, as Go's `compress/gzip` writes them).
#[test]
fn render_writes_text_and_gzip() {
    let families = one_gauge().gather();
    let plain = render(ExpfmtVersion::V0_71_0, &families, None, &[], None);
    assert_eq!(
        plain.content_type,
        "text/plain; version=0.0.4; charset=utf-8; escaping=underscores"
    );
    assert_eq!(plain.content_encoding, None);
    assert_eq!(plain.body, ONE_GAUGE.as_bytes());

    let gzipped = render(
        ExpfmtVersion::V0_71_0,
        &families,
        Some("text/plain;escaping=allow-utf-8"),
        &["gzip"],
        None,
    );
    assert_eq!(
        gzipped.content_type,
        "text/plain; version=0.0.4; charset=utf-8; escaping=allow-utf-8"
    );
    assert_eq!(gzipped.content_encoding, Some("gzip"));
    assert_eq!(
        gzipped.body[..10],
        [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff]
    );
    let mut decoded = String::new();
    GzDecoder::new(gzipped.body.as_slice())
        .read_to_string(&mut decoded)
        .expect("gunzip");
    assert_eq!(decoded, ONE_GAUGE);
}

/// promhttp's `name[]` filter keeps only the families named; an unknown name selects nothing.
#[test]
fn render_filters_by_name() {
    let families = one_gauge().gather();
    let names = BTreeSet::from(["o".to_owned(), "missing".to_owned()]);
    assert_eq!(
        render(ExpfmtVersion::V0_71_0, &families, None, &[], Some(&names)).body,
        b"# HELP o other\n# TYPE o gauge\no 0\n"
    );
    let none = BTreeSet::from(["missing".to_owned()]);
    assert!(
        render(ExpfmtVersion::V0_71_0, &families, None, &[], Some(&none))
            .body
            .is_empty()
    );
}
