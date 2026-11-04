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

//! Go's `encoding/json` decoding of the Engine's answers, for the types a program declares with
//! only the fields it reads: a missing field or a JSON `null` is the zero value
//! ([`null_default`]), unknown fields are ignored, and a list's `null` element is the zero value
//! ([`decode_list`]). A field Go holds behind a pointer is an `Option`, so its presence stays
//! visible.
//!
//! Extracted from swarm-scheduler-exporter/src/docker/types.rs at c9e79afa8810 (the generic
//! part, with the typed decode errors).
//! Extracted from swarm-device-access/src/docker/types.rs at 8b4ab3a3e417 (`null_default` made
//! public for its own types).

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};

use crate::error::Error;

/// Labels, as the daemon sends them.
pub type Labels = BTreeMap<String, String>;

/// Go decodes a JSON `null` into the zero value; derived serde would refuse it. For
/// `#[serde(deserialize_with = "null_default")]` on a field.
///
/// # Errors
///
/// The deserializer's, for a value that is neither `null` nor a `T`.
pub fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Decodes the first JSON value of `body` the way Go's `json.NewDecoder(body).Decode(&out)`
/// does: leading whitespace skipped, whatever follows the value ignored, a `null` the zero value,
/// and an empty body `EOF`.
///
/// # Errors
///
/// [`crate::ErrorKind::Decode`]: `EOF` for an empty body, `unexpected EOF` for one cut inside the
/// value, else serde's text, with the `serde_json::Error` as the source.
pub fn decode_first<T>(body: &[u8]) -> Result<T, Error>
where
    T: Default + for<'de> Deserialize<'de>,
{
    match serde_json::Deserializer::from_slice(body)
        .into_iter::<Option<T>>()
        .next()
    {
        None => Err(Error::decode("EOF".to_owned())),
        Some(Ok(value)) => Ok(value.unwrap_or_default()),
        Some(Err(error)) => {
            let message = if error.is_eof() {
                "unexpected EOF".to_owned()
            } else {
                error.to_string()
            };
            Err(Error::decode(message).with_source(error))
        }
    }
}

/// Decodes a list the way Go decodes it into a slice: `null` is an empty list and a `null`
/// element the zero value.
///
/// # Errors
///
/// As [`decode_first`].
pub fn decode_list<T>(body: &[u8]) -> Result<Vec<T>, Error>
where
    T: Default + for<'de> Deserialize<'de>,
{
    decode_first::<Vec<Nullable<T>>>(body)
        .map(|items| items.into_iter().map(|item| item.0).collect())
}

/// A value Go would decode from `null` into the zero value: a list element, a map value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nullable<T>(pub T);

impl<'de, T: Default + Deserialize<'de>> Deserialize<'de> for Nullable<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        null_default(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests;
