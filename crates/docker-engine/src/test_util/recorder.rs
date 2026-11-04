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

//! A recorded request's target taken apart and normalised as the Go engine-wire tests'
//! `requestRecorder` does: the API version prefix split off, the query decoded as Go's
//! `url.ParseQuery` decodes it, and a normalised form without what legitimately varies between
//! two clients (the version prefix, the encoding of `filters`, the values of `since` and
//! `until`).
//!
//! Extracted from swarm-scheduler-exporter/tests/support/fake_engine.rs at c9e79afa8810
//! (`record`, `split_version_prefix`, `parse_query`, `percent_decode`, `normalize`,
//! `canonical_filters`).

use std::collections::BTreeMap;

use serde_json::Value;

/// A request target, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The path, percent-decoded.
    pub raw_path: String,
    /// The query as sent, without the `?`.
    pub raw_query: String,
    /// `/v1.56`, or empty for an unversioned request such as `/_ping`.
    pub version_prefix: String,
    /// The path without the version prefix.
    pub path: String,
    /// The query pairs, decoded, in their order.
    pub query: Vec<(String, String)>,
}

impl Target {
    /// Takes `target` (`/v1.56/events?since=…`) apart.
    #[must_use]
    pub fn parse(target: &str) -> Self {
        let (raw_path, raw_query) = target
            .split_once('?')
            .map_or((target, ""), |(path, query)| (path, query));
        let raw_path = percent_decode(raw_path, false);
        let (version_prefix, path) = split_version_prefix(&raw_path);
        Self {
            query: parse_query(raw_query),
            raw_path,
            raw_query: raw_query.to_owned(),
            version_prefix,
            path,
        }
    }

    /// The first value of `key`, as Go's `url.Values.Get`.
    #[must_use]
    pub fn query_value(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// The request's normalised form with `method` ([`normalize`]).
    #[must_use]
    pub fn normalized(&self, method: &str) -> String {
        normalize(method, &self.path, &self.query)
    }
}

/// `^(/v\d+\.\d+)(/.*)$`: the version prefix and the rest, or no prefix and the whole path.
#[must_use]
pub fn split_version_prefix(path: &str) -> (String, String) {
    let versioned = path.strip_prefix("/v").and_then(|rest| {
        let slash = rest.find('/')?;
        let (major, minor) = rest[..slash].split_once('.')?;
        let digits =
            |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        (digits(major) && digits(minor)).then_some(slash + 2)
    });
    match versioned {
        Some(split) => (path[..split].to_owned(), path[split..].to_owned()),
        None => (String::new(), path.to_owned()),
    }
}

/// Go's `url.ParseQuery`, pairs in order.
#[must_use]
pub fn parse_query(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key, true), percent_decode(value, true))
        })
        .collect()
}

/// `%XX` decoding (a malformed escape kept as it is), and `+` as a space in a query.
#[must_use]
pub fn percent_decode(text: &str, plus_is_space: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = bytes
                    .get(index + 1..index + 3)
                    .and_then(|hex| std::str::from_utf8(hex).ok())
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                if let Some(byte) = hex {
                    out.push(byte);
                    index += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' if plus_is_space => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The request without what legitimately varies: the version prefix (the caller passes the path
/// without it), the encoding of `filters` (decoded to a sorted canonical form) and the values of
/// `since` and `until`; the pairs sorted by key.
#[must_use]
pub fn normalize(method: &str, path: &str, query: &[(String, String)]) -> String {
    let mut pairs: Vec<(&str, String)> = query
        .iter()
        .map(|(key, value)| {
            let value = match key.as_str() {
                "filters" => canonical_filters(value),
                "since" | "until" => "<time>".to_owned(),
                _ => value.clone(),
            };
            (key.as_str(), value)
        })
        .collect();
    pairs.sort_by(|left, right| left.0.cmp(right.0));
    if pairs.is_empty() {
        return format!("{method} {path}");
    }
    let joined: Vec<String> = pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    format!("{method} {path}?{}", joined.join("&"))
}

/// Both filter encodings the Engine API has used, `{"key":{"value":true}}` and the legacy
/// `{"key":["value"]}`, as `{key:[value,…];…}`.
#[must_use]
pub fn canonical_filters(raw: &str) -> String {
    let Ok(Value::Object(terms)) = serde_json::from_str::<Value>(raw) else {
        return format!("<invalid filters {raw}>");
    };
    let mut decoded: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, values) in terms {
        let mut list: Vec<String> = match values {
            Value::Object(set) => set.keys().cloned().collect(),
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().unwrap_or("<invalid>").to_owned())
                .collect(),
            other @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => {
                vec![format!("<invalid {other}>")]
            }
        };
        list.sort();
        decoded.insert(key, list);
    }
    let parts: Vec<String> = decoded
        .iter()
        .map(|(key, values)| format!("{key}:[{}]", values.join(",")))
        .collect();
    format!("{{{}}}", parts.join(";"))
}
