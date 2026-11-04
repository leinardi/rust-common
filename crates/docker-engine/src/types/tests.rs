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

//! Go's `encoding/json` on the Engine's answers, on a type declared as the programs declare
//! theirs: the generic decoding tests of both programs, whose own types stay with them.
//!
//! Extracted from swarm-scheduler-exporter/src/docker/types/tests.rs at c9e79afa8810
//! (`null_lists_and_elements_are_empty_and_zero`, `decode_first_reads_like_a_go_decoder`).
//! Extracted from swarm-device-access/src/docker/types/tests.rs at 8b4ab3a3e417 (the same
//! two, on its container summary).

use serde::Deserialize;

use super::{Labels, Nullable, decode_first, decode_list, null_default};
use crate::ErrorKind;

/// A container summary as a program declares one: the fields it reads, each Go's zero value
/// when missing or `null`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct Summary {
    #[serde(rename = "Id", deserialize_with = "null_default")]
    id: String,
    #[serde(rename = "Labels", deserialize_with = "null_default")]
    labels: Labels,
    #[serde(rename = "Pid", deserialize_with = "null_default")]
    pid: i64,
}

#[test]
fn null_lists_and_elements_are_empty_and_zero() {
    assert_eq!(decode_list::<Summary>(b"null").unwrap(), []);
    assert_eq!(decode_list::<Summary>(b" []").unwrap(), []);
    let containers = decode_list::<Summary>(br#"[null,{"Id":"c","Labels":null}]"#).unwrap();
    assert_eq!(containers[0], Summary::default());
    assert_eq!(containers[1].id, "c");
    assert!(containers[1].labels.is_empty());
    let summary: Summary = decode_first(br#"{"Id":null,"Pid":null}"#).unwrap();
    assert_eq!(summary, Summary::default());
    let elements: Vec<Nullable<i64>> = serde_json::from_str("[null,3]").unwrap();
    assert_eq!(elements, [Nullable(0), Nullable(3)]);
}

/// Go's `json.NewDecoder(body).Decode` reads the first value and ignores the rest; a failure is
/// a decode error worded as Go's decoder words it, with serde's error kept as its cause.
#[test]
fn decode_first_reads_like_a_go_decoder() {
    assert_eq!(
        decode_first::<Vec<Summary>>(b"  [] trailing garbage").unwrap(),
        []
    );
    assert_eq!(decode_first::<Summary>(b"").unwrap_err().to_string(), "EOF");
    assert_eq!(
        decode_first::<Summary>(b" \n\t").unwrap_err().to_string(),
        "EOF"
    );
    let cut = decode_first::<Summary>(br#"{"Id":"n"#).unwrap_err();
    assert_eq!(cut.to_string(), "unexpected EOF");
    assert_eq!(cut.kind(), ErrorKind::Decode);
    let cause = std::error::Error::source(&cut)
        .and_then(|source| source.downcast_ref::<serde_json::Error>())
        .expect("the serde_json error");
    assert!(cause.is_eof());
    let wrong = decode_first::<Summary>(br#"{"Id":5}"#).unwrap_err();
    assert_eq!(wrong.kind(), ErrorKind::Decode);
    assert_eq!(
        wrong.to_string(),
        "invalid type: integer `5`, expected a string at line 1 column 7"
    );
    assert!(std::error::Error::source(&wrong).is_some());
    assert!(decode_first::<Summary>(br#"{"Pid":"one"}"#).is_err());
    assert!(decode_list::<Summary>(br#"[{"Id":5}]"#).is_err());
}
