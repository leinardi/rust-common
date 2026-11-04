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

//! Go's `TestSplitHostPort` and `TestSplitHostPortErrors` cases, from the net package's tests.
//!
//! Extracted from swarm-device-access/src/go_compat/host_port/tests.rs at 8b4ab3a3e417.

use super::*;

/// The well-formed cases of Go's `splitJoinTests`.
#[test]
fn splits_as_go() {
    for (host_port, host, port) in [
        ("www.google.com:80", "www.google.com", "80"),
        ("127.0.0.1:1234", "127.0.0.1", "1234"),
        ("[::1]:80", "::1", "80"),
        ("[fe80::1%lo0]:80", "fe80::1%lo0", "80"),
        ("localhost%lo0:80", "localhost%lo0", "80"),
        (":8888", "", "8888"),
        ("[::]:8888", "::", "8888"),
        ("host:", "host", ""),
        (":", "", ""),
        ("[]:", "", ""),
    ] {
        assert_eq!(split_host_port(host_port), Ok((host, port)), "{host_port}");
    }
}

/// Go's `splitFailureTests`, with their messages.
#[test]
fn fails_as_go() {
    for (host_port, message) in [
        ("www.google.com", "missing port in address"),
        ("127.0.0.1", "missing port in address"),
        ("[::1]", "missing port in address"),
        ("[fe80::1%lo0]", "missing port in address"),
        ("[localhost%lo0]", "missing port in address"),
        ("localhost%lo0", "missing port in address"),
        ("::1", "too many colons in address"),
        ("fe80::1%lo0", "too many colons in address"),
        ("fe80::1%lo0:80", "too many colons in address"),
        ("[foo:bar]", "missing port in address"),
        ("[foo:bar]baz", "missing port in address"),
        ("[foo]bar:baz", "missing port in address"),
        ("[foo]:[bar]:baz", "too many colons in address"),
        ("[foo]:[bar]baz", "unexpected '[' in address"),
        ("foo[bar]:baz", "unexpected '[' in address"),
        ("foo]bar:baz", "unexpected ']' in address"),
        ("[::1:80", "missing ']' in address"),
    ] {
        let error = split_host_port(host_port).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("address {host_port}: {message}"),
            "{host_port}"
        );
    }
    assert_eq!(
        split_host_port("").unwrap_err().to_string(),
        "missing port in address"
    );
}

#[test]
fn joins_as_go() {
    assert_eq!(join_host_port("127.0.0.1", "80"), "127.0.0.1:80");
    assert_eq!(join_host_port("::1", "80"), "[::1]:80");
    assert_eq!(join_host_port("", "80"), ":80");
}
