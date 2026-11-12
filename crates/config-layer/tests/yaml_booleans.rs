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

//! Which scalars spelled `true` or `false` [`yaml::parse`] reads as booleans, under each
//! [`Booleans`], as values and as mapping keys. The [`Booleans::GoYamlV3`] rows are what koanf
//! with yaml.v3 (knadh/koanf v2, parsers/yaml) loaded from the same text.

use config_layer::yaml::{self, Booleans};
use config_layer::{Location, Map, Origin, Raw};

/// Every mixed-case spelling, quoted and not, directly, through an alias and in flow
/// collections.
const VALUES: &str = "\
a: tRuE
b: True
c: TRUE
d: true
e: FaLsE
f: \"tRuE\"
g: yes
h: &x TrUe
i: *x
l: [tRuE, True, {c: fAlSe}]
";

fn text(value: &str) -> Raw {
    Raw::Text(value.to_owned())
}

/// The value at a path through mappings, `[n]` for a list item.
#[expect(
    clippy::expect_used,
    reason = "a test helper: a missing path fails the test"
)]
fn at<'map>(map: &'map Map, path: &[&str]) -> &'map Raw {
    let (first, rest) = path.split_first().expect("a path");
    let mut value = &map.get(*first).unwrap_or_else(|| panic!("{first}")).value;
    for step in rest {
        value = match (value, step.strip_prefix('[')) {
            (Raw::List(items), Some(index)) => {
                let index: usize = index.trim_end_matches(']').parse().expect("an index");
                &items[index].value
            }
            (Raw::Map(entries), None) => {
                &entries.get(*step).unwrap_or_else(|| panic!("{step}")).value
            }
            (other, _) => panic!("{step} in {other:?}"),
        };
    }
    value
}

#[test]
fn go_yaml_v3_reads_only_its_six_spellings_as_booleans() {
    let map = yaml::parse(VALUES, Booleans::GoYamlV3).expect("valid YAML");
    for (path, want) in [
        (&["a"][..], text("tRuE")),
        (&["b"], Raw::Bool(true)),
        (&["c"], Raw::Bool(true)),
        (&["d"], Raw::Bool(true)),
        (&["e"], text("FaLsE")),
        (&["f"], text("tRuE")),
        (&["g"], text("yes")),
        (&["h"], text("TrUe")),
        (&["i"], text("TrUe")),
        (&["l", "[0]"], text("tRuE")),
        (&["l", "[1]"], Raw::Bool(true)),
        (&["l", "[2]", "c"], text("fAlSe")),
    ] {
        assert_eq!(at(&map, path), &want, "{path:?}");
    }
    assert_eq!(
        map["a"].origin,
        Origin::Yaml(Location { line: 1, column: 4 }),
        "a resolved scalar keeps its location"
    );
}

#[test]
fn any_case_reads_true_and_false_in_any_case_as_booleans() {
    let map = yaml::parse(VALUES, Booleans::AnyCase).expect("valid YAML");
    for (path, want) in [
        (&["a"][..], Raw::Bool(true)),
        (&["b"], Raw::Bool(true)),
        (&["c"], Raw::Bool(true)),
        (&["d"], Raw::Bool(true)),
        (&["e"], Raw::Bool(false)),
        (&["f"], text("tRuE")),
        (&["g"], text("yes")),
        (&["h"], Raw::Bool(true)),
        (&["i"], Raw::Bool(true)),
        (&["l", "[0]"], Raw::Bool(true)),
        (&["l", "[1]"], Raw::Bool(true)),
        (&["l", "[2]", "c"], Raw::Bool(false)),
    ] {
        assert_eq!(at(&map, path), &want, "{path:?}");
    }
}

#[test]
fn a_mixed_case_key_is_text_only_under_go_yaml_v3() {
    let source = "labels:\n  tRuE: x\n  FaLsE: y\nflow: {TrUe: z}\n";
    let map = yaml::parse(source, Booleans::GoYamlV3).expect("valid YAML");
    assert_eq!(at(&map, &["labels", "tRuE"]), &text("x"));
    assert_eq!(at(&map, &["labels", "FaLsE"]), &text("y"));
    assert_eq!(at(&map, &["flow", "TrUe"]), &text("z"));
    assert_eq!(
        yaml::parse(source, Booleans::AnyCase)
            .expect_err("a boolean key")
            .to_string(),
        "a mapping key must be text, not a boolean at line 2, column 3"
    );
}

/// Go's koanf takes a nested such key as its `%v` text (`true`) and a top-level one as written;
/// refusing it in both is a difference the consumer records. The location is serde-saphyr's, as for a key of any other kind.
#[test]
fn a_key_in_a_boolean_spelling_is_refused_under_both() {
    for booleans in [Booleans::GoYamlV3, Booleans::AnyCase] {
        for (source, location) in [
            ("labels:\n  True: x\n", Some((2, 3))),
            ("labels: {a: 1, false: 2}\n", Some((1, 9))),
            ("? TRUE\n: x\n", None),
        ] {
            let error = yaml::parse(source, booleans).expect_err("a boolean key");
            let at = location.map_or(String::new(), |(line, column)| {
                format!(" at line {line}, column {column}")
            });
            assert_eq!(
                error.to_string(),
                format!("a mapping key must be text, not a boolean{at}"),
                "{booleans:?} {source:?}"
            );
            assert_eq!(
                error.location(),
                location.map(|(line, column)| Location { line, column })
            );
        }
        let quoted = yaml::parse("labels:\n  \"true\": x\n", booleans).expect("a quoted key");
        assert_eq!(at(&quoted, &["labels", "true"]), &text("x"), "{booleans:?}");
    }
}

/// An anchored key and an alias used as a key read as the anchored scalar's spelling.
#[test]
fn anchored_and_aliased_keys_keep_their_spelling() {
    let map = yaml::parse(
        "k: &k tRuE\nm:\n  *k : 1\nn:\n  &j FaLsE : 2\n",
        Booleans::GoYamlV3,
    )
    .expect("valid YAML");
    assert_eq!(at(&map, &["k"]), &text("tRuE"));
    assert_eq!(at(&map, &["m", "tRuE"]), &Raw::Int(1));
    assert_eq!(at(&map, &["n", "FaLsE"]), &Raw::Int(2));
}

/// Byte offsets after a byte-order mark, a multi-byte key and CRLF line ends still find the
/// spelling.
#[test]
fn the_spelling_is_read_after_multibyte_text_and_crlf() {
    let source = "\u{feff}schlüssel: ünd\r\nwert: tRuE\r\nzwei: TRUE\r\n";
    let map = yaml::parse(source, Booleans::GoYamlV3).expect("valid YAML");
    assert_eq!(at(&map, &["wert"]), &text("tRuE"));
    assert_eq!(at(&map, &["zwei"]), &Raw::Bool(true));
}

#[test]
fn a_mixed_case_document_is_text_at_the_top() {
    assert_eq!(
        yaml::parse("tRuE\n", Booleans::GoYamlV3)
            .expect_err("not a mapping")
            .to_string(),
        "the configuration must be a mapping, not text"
    );
    assert_eq!(
        yaml::parse("tRuE\n", Booleans::AnyCase)
            .expect_err("not a mapping")
            .to_string(),
        "the configuration must be a mapping, not a boolean"
    );
}

/// The one difference from yaml.v3 left: it refuses an explicit `!!bool` on another spelling,
/// which is text here, plain or quoted; one of its spellings is a boolean, quoted or not, and a
/// quoted one with an escape keeps serde-saphyr's reading.
#[test]
fn an_explicit_bool_tag_keeps_the_spelling_rule() {
    let map = yaml::parse(
        "a: !!bool tRuE\nb: !!bool TRUE\nc: !!bool \"true\"\nd: !!bool 'True'\n\
         e: !!bool \"tRuE\"\nf: !!bool \"\\x74rue\"\n",
        Booleans::GoYamlV3,
    )
    .expect("valid YAML");
    for (key, want) in [
        ("a", text("tRuE")),
        ("b", Raw::Bool(true)),
        ("c", Raw::Bool(true)),
        ("d", Raw::Bool(true)),
        ("e", text("tRuE")),
        ("f", Raw::Bool(true)),
    ] {
        assert_eq!(at(&map, &[key]), &want, "{key}");
    }
}
