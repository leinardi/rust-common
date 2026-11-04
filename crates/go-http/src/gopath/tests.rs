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

//! The routing cases are Go 1.26's answers in the wire captures; the rest are Go's own results
//! (`path.Clean`'s tests, and a throwaway Go program for `URL.String` and `ParseQuery`).
//!
//! Extracted from swarm-scheduler-exporter/src/server/gopath/tests.rs at c9e79afa8810 (the same
//! as swarm-device-access's); `path.Clean`'s cases now run against `go_compat::filepath::clean`.

use super::*;

const PATTERNS: &[&str] = &["/metrics", "/healthz"];

fn routed(target: &str) -> Route {
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    };
    route(path, query, PATTERNS)
}

/// The mux's answer to every routing case of the captures.
#[test]
fn routes_as_go() {
    assert_eq!(routed("/metrics"), Route::Matched("/metrics"));
    assert_eq!(routed("/healthz?"), Route::Matched("/healthz"));
    assert_eq!(routed("/%68ealthz"), Route::Matched("/healthz"));
    for target in ["/", "/nope", "/metrics/", "/healthz/", "/a%2F..%2Fhealthz"] {
        assert_eq!(routed(target), Route::NotFound, "{target}");
    }
    for (target, location) in [
        ("//metrics", "/metrics"),
        ("/a/../metrics", "/metrics"),
        ("/./healthz?x=1", "/healthz?x=1"),
        ("/metrics/..", "/"),
        ("/healthz/.", "/healthz"),
        ("/a%20b/../healthz", "/healthz"),
        ("//%68ealthz", "/%2568ealthz"),
    ] {
        assert_eq!(
            routed(target),
            Route::Redirect(location.to_owned()),
            "{target}"
        );
    }
    assert_eq!(routed("/%zz"), Route::BadRequest);
    assert_eq!(routed("/%4"), Route::BadRequest);
}

/// `path.Clean`'s cases.
#[test]
fn path_clean_is_go_s() {
    for (input, want) in [
        ("", "."),
        ("/", "/"),
        ("abc", "abc"),
        ("a//b/./c/..", "a/b"),
        ("/../a", "/a"),
        ("../../a", "../../a"),
        ("/a/b/", "/a/b"),
        ("a/../..", ".."),
        ("a/..", "."),
    ] {
        assert_eq!(filepath::clean(input), want, "{input}");
    }
    assert_eq!(clean_path(""), "/");
    assert_eq!(clean_path("a/b/"), "/a/b/");
}

/// `URL.String`'s host escaping, from a Go program.
#[test]
fn escape_host_is_go_s() {
    for (host, want) in [
        ("127.0.0.1:8888", "127.0.0.1:8888"),
        ("[::1]:8888", "[::1]:8888"),
        ("a b:1", "a%20b:1"),
        ("ex%ample:1", "ex%25ample:1"),
        ("h\u{e9}llo:1", "h%C3%A9llo:1"),
        ("x<y>:1", "x<y>:1"),
    ] {
        assert_eq!(escape_host(host), want, "{host}");
    }
}

/// `url.ParseQuery`, from a Go program: an escaped key, `+` as a space, an empty pair and a
/// value-less key kept, and a pair with a `;` dropped.
#[test]
fn parse_query_is_go_s() {
    let parsed = parse_query("name[]=a&name%5B%5D=b&x;y=1&name[]=c+d&&name[]");
    let want: Vec<Vec<u8>> = vec![b"a".to_vec(), b"b".to_vec(), b"c d".to_vec(), Vec::new()];
    assert_eq!(parsed.get(b"name[]".as_slice()), Some(&want));
    assert_eq!(parsed.len(), 1);
    assert!(parse_query("").is_empty());
    assert!(parse_query("a=%zz").is_empty());
}

#[test]
fn html_escape_is_go_s() {
    assert_eq!(
        html_escape("/a?b=<c>&d=\"e\"'"),
        "/a?b=&lt;c&gt;&amp;d=&#34;e&#34;&#39;"
    );
}
