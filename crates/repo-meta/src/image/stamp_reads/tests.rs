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

//! The reads a version module makes, each form with its negative controls.

use super::stamp_reads;

/// The three reads of service-kit's `buildinfo!` under `APP`.
const APP: [&str; 3] = ["APP_COMMIT", "APP_DATE", "APP_VERSION"];

/// The reads of `text`, sorted.
fn reads(text: &str) -> Vec<String> {
    stamp_reads(text).into_iter().collect()
}

/// swarm-device-access's import of `buildinfo!`.
const IMPORT: &str = "use service_kit::{BuildInfo, Defaults, EmptyStamp, buildinfo};";

/// swarm-device-access's src/version.rs, as it calls service-kit.
const IMPORTED: &str = "use service_kit::{BuildInfo, Defaults, EmptyStamp, buildinfo};\n\
                        \n\
                        pub const BUILD: BuildInfo = buildinfo!(\n\
                        \x20   \x22APP\x22,\n\
                        \x20   Defaults { version: \x22dev\x22, commit: \x22none\x22, date: \
                        \x22unknown\x22, empty: EmptyStamp::Kept },\n\
                        );\n";

#[test]
fn literal_reads_count() {
    assert_eq!(
        reads(
            "stamp(option_env!(\x22APP_VERSION\x22))\n\
             ::core::option_env!(\x22APP_COMMIT\x22,)\n\
             std::option_env!(r#\x22APP_DATE\x22#)\n"
        ),
        APP
    );
}

#[test]
fn literal_reads_in_comments_strings_or_other_macros_do_not_count() {
    for text in [
        "// option_env!(\x22APP_VERSION\x22)\n",
        "/// option_env!(\x22APP_VERSION\x22)\n",
        "/* outer /* nested */ option_env!(\x22APP_VERSION\x22) */\n",
        "const TEXT: &str = \x22option_env!(\\\x22APP_VERSION\\\x22)\x22;\n",
        "const TEXT: &str = r#\x22option_env!(\x22APP_VERSION\x22)\x22#;\n",
        "mine::option_env!(\x22APP_VERSION\x22)\n",
        "option_env!(concat!(\x22APP\x22, \x22_VERSION\x22))\n",
    ] {
        assert_eq!(reads(text), Vec::<String>::new(), "{text:?}");
    }
}

#[test]
fn service_kit_buildinfo_reads_its_fields() {
    for text in [
        IMPORTED.to_owned(),
        IMPORTED.replace(
            "{BuildInfo, Defaults, EmptyStamp, buildinfo}",
            "{buildinfo, BuildInfo}",
        ),
        "use service_kit::buildinfo;\nconst B: B = buildinfo!(\x22APP\x22, D);\n".to_owned(),
        "use ::service_kit::buildinfo;\nconst B: B = buildinfo![r\x22APP\x22, D];\n".to_owned(),
        "const B: B = service_kit::buildinfo!(\x22APP\x22, D);\n".to_owned(),
        "const B: B = ::service_kit::buildinfo!(\x22APP\x22, D,);\n".to_owned(),
        // A lifetime and character literals before the call do not swallow it.
        "fn f<'a>(_: &'a str) -> char { '\\'' }\nconst Q: u8 = b'\x22';\n\
         const B: B = service_kit::buildinfo!(\x22APP\x22, D);\n"
            .to_owned(),
    ] {
        assert_eq!(reads(&text), APP, "{text:?}");
    }
}

#[test]
fn a_buildinfo_that_is_not_service_kits_reads_nothing() {
    for (name, text) in [
        ("not imported", IMPORTED.replace(IMPORT, "")),
        (
            "imported from another crate",
            IMPORTED.replace("use service_kit::", "use other_kit::"),
        ),
        (
            "imported under another name",
            "use service_kit::buildinfo as other;\nconst B: B = buildinfo!(\x22APP\x22, D);\n"
                .to_owned(),
        ),
        (
            "only a nested path imported",
            IMPORTED.replace(
                "{BuildInfo, Defaults, EmptyStamp, buildinfo}",
                "{BuildInfo, inner::{buildinfo}}",
            ),
        ),
        (
            "the import commented out",
            IMPORTED.replace("use service_kit", "// use service_kit"),
        ),
        (
            "the import in a block comment",
            IMPORTED
                .replace("use service_kit", "/* /* */ use service_kit")
                .replace("buildinfo};", "buildinfo}; */"),
        ),
        (
            "the import in a string",
            format!(
                "const DOC: &str = \x22use service_kit::buildinfo;\x22;\n{}",
                IMPORTED.replace(IMPORT, "")
            ),
        ),
        (
            "a local macro_rules! shadows the import",
            format!("{IMPORTED}macro_rules! buildinfo {{ ($p:literal, $d:expr) => {{ 0 }} }}\n"),
        ),
        (
            "another crate's buildinfo!",
            "const B: B = other_kit::buildinfo!(\x22APP\x22, D);\n".to_owned(),
        ),
        (
            "a longer path",
            "const B: B = other::service_kit::buildinfo!(\x22APP\x22, D);\n".to_owned(),
        ),
        (
            "$crate's buildinfo!",
            "const B: B = $crate::buildinfo!(\x22APP\x22, D);\n".to_owned(),
        ),
        (
            "the call commented out",
            IMPORTED.replace("pub const BUILD", "// pub const BUILD"),
        ),
        (
            "the call in a nested block comment",
            IMPORTED
                .replace("pub const BUILD", "/* /* inner */ pub const BUILD")
                .replace(");\n", "); */\n"),
        ),
        (
            "the call in a string",
            "use service_kit::buildinfo;\n\
             const DOC: &str = \x22buildinfo!(\\\x22APP\\\x22, D)\x22;\n"
                .to_owned(),
        ),
        (
            "the call in a raw string",
            "use service_kit::buildinfo;\n\
             const DOC: &str = r#\x22buildinfo!(\x22APP\x22, D)\x22#;\n"
                .to_owned(),
        ),
        (
            "a prefix that is not a string literal",
            IMPORTED.replace("\x22APP\x22,", "PREFIX,"),
        ),
        (
            "a byte-string prefix",
            IMPORTED.replace("\x22APP\x22,", "b\x22APP\x22,"),
        ),
    ] {
        assert_eq!(reads(&text), Vec::<String>::new(), "{name}: {text:?}");
    }
}

#[test]
fn a_buildinfo_reads_under_its_own_prefix_only() {
    assert_eq!(
        reads(&IMPORTED.replace("\x22APP\x22", "\x22OTHER\x22")),
        ["OTHER_COMMIT", "OTHER_DATE", "OTHER_VERSION"]
    );
}

#[test]
fn an_unterminated_comment_or_string_hides_the_rest() {
    for text in [
        format!("/* /* */\n{IMPORTED}"),
        format!("const DOC: &str = \x22\n{IMPORTED}"),
        format!("const DOC: &str = r#\x22\x22\n{IMPORTED}"),
    ] {
        assert_eq!(reads(&text), Vec::<String>::new(), "{text:?}");
    }
}
