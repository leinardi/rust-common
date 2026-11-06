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

//! repo-meta's `BUILDINFO_FIELDS`, the fields its stamp check counts a service-kit `buildinfo!`
//! call as reading, are the fields the macro reads. repo-meta sits before service-kit in the
//! README's crate table, so it cannot depend on it and keeps its own copy of the list; this test
//! reads the macro's text in `crates/service-kit/src/stamps.rs` and compares. The negative control
//! runs the same reader on altered copies of the macro.

use repo_meta::image::BUILDINFO_FIELDS;

const STAMPS_RS: &str = "crates/service-kit/src/stamps.rs";

/// What each field read of the macro starts with; the field name follows, up to the quote.
const FIELD_READ: &str = "::core::option_env!(::core::concat!($prefix, \x22_";

/// The fields the `buildinfo!` definition in `text` reads, in order, or why they cannot be told:
/// every `option_env!` in its body must read `concat!($prefix, "_<field>")`.
fn macro_fields(text: &str) -> Result<Vec<String>, String> {
    let start = text
        .find("macro_rules! buildinfo {")
        .ok_or("no macro_rules! buildinfo")?;
    // The definition ends at the first line that closes it at column 0.
    let body = &text[start..];
    let body = &body[..body
        .find("\n}\n")
        .ok_or("macro_rules! buildinfo is not closed")?];
    let reads = body.matches("option_env!").count();
    let fields: Vec<String> = body
        .split(FIELD_READ)
        .skip(1)
        .map(|rest| rest.split('\x22').next().unwrap_or_default().to_owned())
        .collect();
    if fields.len() == reads {
        Ok(fields)
    } else {
        Err(format!(
            "{reads} option_env! reads, {} of them of concat!($prefix, \x22_<field>\x22)",
            fields.len()
        ))
    }
}

#[test]
fn repo_meta_counts_the_fields_buildinfo_reads() {
    let text = meta::repo().read(STAMPS_RS);
    assert_eq!(
        macro_fields(&text),
        Ok(BUILDINFO_FIELDS.map(str::to_owned).to_vec()),
        "repo_meta::image::BUILDINFO_FIELDS and {STAMPS_RS}'s buildinfo! read different fields"
    );
}

#[test]
fn macro_fields_is_not_blind() {
    let text = meta::repo().read(STAMPS_RS);
    let date = "concat!($prefix, \x22_DATE\x22)),\n";
    assert!(text.contains(date), "{date:?} is gone");
    let added = text.replace(
        date,
        &format!(
            "{date}            ::core::option_env!(::core::concat!($prefix, \x22_BRANCH\x22)),\n"
        ),
    );
    assert_eq!(
        macro_fields(&added),
        Ok(["VERSION", "COMMIT", "DATE", "BRANCH"]
            .map(str::to_owned)
            .to_vec())
    );
    assert!(
        macro_fields(&text.replace("\x22_COMMIT\x22", "\x22_SHA\x22"))
            .is_ok_and(|fields| fields != BUILDINFO_FIELDS),
        "a renamed field was not seen"
    );
    assert!(
        macro_fields(&text.replace(
            "::core::option_env!(::core::concat!($prefix, \x22_DATE\x22))",
            "::core::option_env!(\x22FIXED_DATE\x22)"
        ))
        .is_err(),
        "a read of another shape was not seen"
    );
    assert!(macro_fields(&text.replace("macro_rules! buildinfo", "macro_rules! other")).is_err());
}
