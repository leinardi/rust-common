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

//! The secret file read and the problem sources.

use std::path::{Path, PathBuf};

use config_layer::{
    EmptySecret, Location, Node, Origin, Problem, Problems, Raw, SecretError, SecretTrim, Source,
    read_secret_file,
};

const SENTINEL: &str = "s3ntinel-PASSWORD-do-not-print";

#[expect(
    clippy::expect_used,
    reason = "a test helper: a broken fixture fails the test"
)]
fn write(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, bytes).expect("a scratch file");
    path
}

/// Go's two trimming rules: `strings.TrimRight(s, " \t\r\n")` and `strings.TrimSpace`.
#[test]
fn secrets_are_trimmed_by_the_rule_asked_for() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = write(directory.path(), "secret", b"\t  value \x0b\t\r\n");
    let read =
        |trim| read_secret_file("key_file", &path, trim, EmptySecret::Refused).expect("readable");
    assert_eq!(read(SecretTrim::TrailingWhitespace), "\t  value \x0b");
    assert_eq!(read(SecretTrim::Whitespace), "value");

    let blank = write(directory.path(), "blank", b" \r\n");
    assert_eq!(
        read_secret_file(
            "key_file",
            &blank,
            SecretTrim::Whitespace,
            EmptySecret::Allowed
        )
        .expect("an empty secret allowed"),
        ""
    );
    assert!(matches!(
        read_secret_file(
            "key_file",
            &blank,
            SecretTrim::Whitespace,
            EmptySecret::Refused
        ),
        Err(SecretError::Empty { .. })
    ));
}

/// Every error names the key and the path, and none shows what the file holds.
#[test]
fn secret_errors_never_hold_the_content() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let not_utf8 = write(
        directory.path(),
        "not-utf8",
        &[SENTINEL.as_bytes(), b"\xff\xfe"].concat(),
    );
    let blank = write(directory.path(), "blank", b"\n");
    let unreadable = directory.path().join(SENTINEL.len().to_string());
    std::fs::create_dir(&unreadable).expect("a directory where a file is expected");
    let missing = directory.path().join("missing");
    let holder = write(directory.path(), "holder", SENTINEL.as_bytes());
    let under_a_file = holder.join("child");

    for (path, trim) in [
        (&not_utf8, SecretTrim::Whitespace),
        (&blank, SecretTrim::TrailingWhitespace),
        (&unreadable, SecretTrim::Whitespace),
        (&missing, SecretTrim::Whitespace),
        (&under_a_file, SecretTrim::Whitespace),
    ] {
        let error = read_secret_file("registry.password_file", path, trim, EmptySecret::Refused)
            .expect_err("unusable");
        for shown in [error.to_string(), format!("{error:?}")] {
            assert!(!shown.contains(SENTINEL), "{shown}");
            assert!(shown.contains("registry.password_file"), "{shown}");
            assert!(shown.contains(&path.display().to_string()), "{shown}");
        }
    }
}

/// A YAML error's `Debug` and its source chain are one line and hold none of the file's text,
/// though serde-saphyr's own error renders an excerpt.
#[test]
fn yaml_errors_never_hold_the_file_text() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let text = format!("registry:\n  password: {SENTINEL}\nother: 1\nother: 2\n");
    let path = write(directory.path(), "config.yaml", text.as_bytes());
    let error = config_layer::yaml::read_file(
        &path,
        config_layer::yaml::MissingFile::Refused,
        config_layer::yaml::Booleans::AnyCase,
    )
    .expect_err("a duplicate key");
    let mut shown = vec![error.to_string(), format!("{error:?}")];
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        shown.push(cause.to_string());
        shown.push(format!("{cause:?}"));
        source = cause.source();
    }
    for line in shown {
        assert!(!line.contains(SENTINEL), "{line}");
        assert!(!line.contains('\n'), "{line}");
    }
}

/// Every source renders; only the YAML one has a line and column.
#[test]
fn every_source_renders_without_an_invented_location() {
    let file = PathBuf::from("/etc/runhold/config.yaml");
    for (source, want) in [
        (
            Source::Yaml {
                file: file.clone(),
                location: Location { line: 3, column: 5 },
            },
            "/etc/runhold/config.yaml:3:5",
        ),
        (
            Source::YamlFile { file: file.clone() },
            "/etc/runhold/config.yaml",
        ),
        (
            Source::Environment {
                variable: "RUNHOLD_SERVER__LISTEN".to_owned(),
            },
            "environment variable RUNHOLD_SERVER__LISTEN",
        ),
        (
            Source::SecretFile {
                key: "notify.smtp.password_file".to_owned(),
                path: PathBuf::from("/run/secrets/smtp"),
            },
            "file /run/secrets/smtp named by notify.smtp.password_file",
        ),
        (Source::Missing, "not set"),
    ] {
        assert_eq!(source.to_string(), want);
        let problem = Problem::new("a.b", source, "bad");
        assert_eq!(problem.to_string(), format!("a.b: bad ({want})"));
    }

    let yaml = Node::new(Raw::Null, Origin::Yaml(Location { line: 7, column: 2 }));
    assert_eq!(
        Source::of(&yaml, &file),
        Source::Yaml {
            file: file.clone(),
            location: Location { line: 7, column: 2 }
        }
    );
    let env = Node::new(Raw::Null, Origin::Environment("RUNHOLD_X".to_owned()));
    assert_eq!(
        Source::of(&env, &file),
        Source::Environment {
            variable: "RUNHOLD_X".to_owned()
        }
    );
}

/// The collector keeps every problem and sorts by path, stably.
#[test]
fn problems_sort_stably_by_path() {
    let problem = |path: &str, message: &str| Problem::new(path, Source::Missing, message);
    let mut problems: Problems = [problem("b", "first b"), problem("a", "a")]
        .into_iter()
        .collect();
    problems.push(problem("b", "second b"));
    problems.extend([problem("a.c", "a.c")]);
    assert_eq!(problems.len(), 4);
    let sorted: Vec<(String, String)> = problems
        .into_sorted()
        .into_iter()
        .map(|problem| (problem.path, problem.message))
        .collect();
    assert_eq!(
        sorted,
        [
            ("a".to_owned(), "a".to_owned()),
            ("a.c".to_owned(), "a.c".to_owned()),
            ("b".to_owned(), "first b".to_owned()),
            ("b".to_owned(), "second b".to_owned()),
        ]
    );
}
