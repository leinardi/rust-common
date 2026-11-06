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

//! What moby's calls put in a request, and the bounds a caller puts around one: moby's
//! `Filters` as JSON, Go's `url.Values.Encode`, `GetTimestamp`'s `since`, `trimID`, and Go's
//! `context.WithTimeout` and `WithDeadline` around a call that takes no context.
//!
//! Extracted from swarm-scheduler-exporter/src/docker/api.rs at c9e79afa8810 (the generic part;
//! `DockerApi` and the program's timeouts stay in the program).
//! Extracted from swarm-device-access/src/docker/api.rs at 8b4ab3a3e417 (`encode_query`,
//! `push_json_string` and `trim_id` made public for its write calls).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;

use go_compat::quote::push_hex_escape;
use jiff::Timestamp;

use crate::error::Error;
use crate::host::push_percent;

/// moby's `Filters`: terms with sets of values, sent as `{"term":{"value":true}}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    terms: BTreeMap<String, BTreeSet<String>>,
}

impl Filters {
    /// No filters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// moby's `Filters.Add`.
    #[must_use]
    pub fn add(mut self, term: &str, values: &[&str]) -> Self {
        let set = self.terms.entry(term.to_owned()).or_default();
        set.extend(values.iter().map(|value| (*value).to_owned()));
        self
    }

    /// Whether there are none, in which case no `filters` parameter is sent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// The `filters` parameter: Go's `json.Marshal` of the map, keys sorted.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::from("{");
        for (term_index, (term, values)) in self.terms.iter().enumerate() {
            if term_index > 0 {
                out.push(',');
            }
            push_json_string(&mut out, term);
            out.push_str(":{");
            for (value_index, value) in values.iter().enumerate() {
                if value_index > 0 {
                    out.push(',');
                }
                push_json_string(&mut out, value);
                out.push_str(":true");
            }
            out.push('}');
        }
        out.push('}');
        out
    }
}

/// encoding/json's string encoding with `json.Marshal`'s HTML escaping, quotes included.
pub fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' | '\\' => {
                out.push('\\');
                out.push(character);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' | '\0'..='\u{1f}' => {
                push_hex_escape(out, 'u', u32::from(character), 4);
            }
            _ => out.push(character),
        }
    }
    out.push('"');
}

/// Go's `url.Values.Encode`: the pairs sorted by key, each side `url.QueryEscape`d.
#[must_use]
pub fn encode_query(pairs: &mut [(&str, String)]) -> String {
    pairs.sort_by(|left, right| left.0.cmp(right.0));
    let mut out = String::new();
    for (key, value) in pairs.iter() {
        if !out.is_empty() {
            out.push('&');
        }
        push_query_escaped(&mut out, key);
        out.push('=');
        push_query_escaped(&mut out, value);
    }
    out
}

/// Go's `url.QueryEscape`: the unreserved bytes kept, a space as `+`, the rest `%XX`.
fn push_query_escaped(out: &mut String, text: &str) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else if byte == b' ' {
            out.push('+');
        } else {
            push_percent(out, byte);
        }
    }
}

/// moby's `GetTimestamp` of an RFC 3339 time passed as `since`: `<seconds>.<nanoseconds>`, nine
/// digits, the seconds rounded down.
#[must_use]
pub fn since_parameter(since: Timestamp) -> String {
    let mut seconds = since.as_second();
    let mut nanos = since.subsec_nanosecond();
    if nanos < 0 {
        seconds -= 1;
        nanos += 1_000_000_000;
    }
    format!("{seconds}.{nanos:09}")
}

/// moby's `trimID`: surrounding whitespace dropped, an empty ID refused (`kind` is `container`,
/// `service`, …).
///
/// # Errors
///
/// [`crate::ErrorKind::InvalidParameter`], `invalid <kind> name or ID: value is empty`.
pub fn trim_id<'id>(kind: &str, id: &'id str) -> Result<&'id str, Error> {
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::invalid_parameter(format!(
            "invalid {kind} name or ID: value is empty"
        )));
    }
    Ok(id)
}

/// Bounds `call` by `timeout`, as `context.WithTimeout` bounds a Go call, with the bare error:
/// the client does not see the deadline, so a request cut short ends as `context deadline
/// exceeded`.
///
/// **Not [`crate::with_context_timeout`].** That one hands the deadline to the client, so a
/// request it cuts short fails as net/http fails it, `Get "<url>": context deadline exceeded`.
/// swarm-scheduler-exporter's `with_timeout` is this function; swarm-device-access's
/// `docker::with_timeout` is [`crate::with_context_timeout`], and must move to that one to keep
/// its error text. The name says `bare` so that the two are never swapped by an import.
///
/// # Errors
///
/// The call's [`Error`], or [`Error::deadline_exceeded`] when the time ran out first.
pub async fn with_bare_timeout<T>(
    timeout: Duration,
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(timeout, call)
        .await
        .unwrap_or_else(|_| Err(Error::deadline_exceeded()))
}

/// Bounds `call` by a `deadline` other calls share, with the bare error as
/// [`with_bare_timeout`] (swarm-scheduler-exporter's `with_deadline`). A context with a deadline,
/// [`crate::CallContext::with_deadline`] under [`crate::with_context`], gives net/http's error
/// instead.
///
/// # Errors
///
/// The call's [`Error`], or [`Error::deadline_exceeded`] when the deadline passed first.
pub async fn with_bare_deadline<T>(
    deadline: tokio::time::Instant,
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout_at(deadline, call)
        .await
        .unwrap_or_else(|_| Err(Error::deadline_exceeded()))
}

#[cfg(test)]
mod tests;
