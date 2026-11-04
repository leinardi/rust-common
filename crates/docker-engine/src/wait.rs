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

//! The answer of moby's `ContainerWait`, whatever its condition (`not-running`, `next-exit`,
//! `removed`), read as moby's wait goroutine reads it: the first JSON value of the body, or the
//! error its channel carries, worded as moby words it.
//!
//! Extracted from swarm-device-access/src/launcher/engine.rs at 8b4ab3a3e417 (`wait_answer`,
//! `syntax_error_text`, `decode_wait`, `WaitResponse`, `WaitExitError`).

use http_body_util::BodyExt as _;
use serde::Deserialize;

use crate::client::Body;
use crate::types::null_default;

/// moby's `containerWaitErrorMsgLimit`: how much more of a wait answer that is not JSON is read
/// for the error.
pub const WAIT_ERROR_LIMIT: usize = 2 * 1024;

/// container `WaitResponse`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WaitResponse {
    /// `StatusCode`.
    #[serde(rename = "StatusCode", deserialize_with = "null_default")]
    pub status_code: i64,
    /// `Error`, absent unless the wait itself failed.
    #[serde(rename = "Error")]
    pub error: Option<WaitExitError>,
}

/// container `WaitExitError`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WaitExitError {
    /// `Message`.
    #[serde(rename = "Message", deserialize_with = "null_default")]
    pub message: String,
}

/// moby's wait goroutine on the answer `body` of a wait the Engine accepted: the first JSON value
/// (a [`WaitResponse`], or a caller's type with the fields it reads), or the error its channel
/// carries. A syntax error is reported as the text of the answer (up to [`WAIT_ERROR_LIMIT`] more
/// of it), since a proxy that cuts the stream can only write plain text after the status; any
/// other failure as it reads.
///
/// # Errors
///
/// moby's text: `EOF` for an answer with nothing but whitespace, `unexpected EOF` for one cut
/// inside the value, the answer's text for one that is not JSON, the body's or the decoder's
/// error otherwise.
pub async fn wait_answer<T>(mut body: Body) -> Result<T, String>
where
    T: Default + for<'de> Deserialize<'de>,
{
    let mut received = Vec::new();
    loop {
        let read = body.frame().await;
        let ended = match read {
            None => true,
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    received.extend_from_slice(&data);
                }
                false
            }
            Some(Err(error)) => {
                return Err(match decode_wait::<T>(&received) {
                    // What was read so far is incomplete: the read error is what stopped it.
                    Decoded::Value(_) | Decoded::Incomplete => error.to_string(),
                    Decoded::Syntax => String::from_utf8_lossy(&received).into_owned(),
                    Decoded::Other(text) => text,
                });
            }
        };
        match decode_wait(&received) {
            Decoded::Value(response) => return Ok(response),
            Decoded::Incomplete if !ended => {}
            Decoded::Incomplete if received.iter().all(u8::is_ascii_whitespace) => {
                return Err("EOF".to_owned());
            }
            Decoded::Incomplete => return Err("unexpected EOF".to_owned()),
            Decoded::Syntax => {
                return Err(syntax_error_text(&mut body, received, ended).await);
            }
            Decoded::Other(text) => return Err(text),
        }
    }
}

/// The answer's text for a syntax error: what was read, and up to [`WAIT_ERROR_LIMIT`] more.
async fn syntax_error_text(body: &mut Body, mut received: Vec<u8>, ended: bool) -> String {
    if !ended {
        let limit = received.len() + WAIT_ERROR_LIMIT;
        while received.len() < limit {
            match body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        received.extend_from_slice(&data);
                    }
                }
                None | Some(Err(_)) => break,
            }
        }
        received.truncate(limit);
    }
    String::from_utf8_lossy(&received).into_owned()
}

enum Decoded<T> {
    Value(T),
    Incomplete,
    Syntax,
    Other(String),
}

fn decode_wait<T>(received: &[u8]) -> Decoded<T>
where
    T: Default + for<'de> Deserialize<'de>,
{
    match serde_json::Deserializer::from_slice(received)
        .into_iter::<Option<T>>()
        .next()
    {
        None => Decoded::Incomplete,
        Some(Ok(value)) => Decoded::Value(value.unwrap_or_default()),
        Some(Err(error)) => match error.classify() {
            serde_json::error::Category::Eof => Decoded::Incomplete,
            serde_json::error::Category::Syntax => Decoded::Syntax,
            serde_json::error::Category::Io | serde_json::error::Category::Data => {
                Decoded::Other(error.to_string())
            }
        },
    }
}

#[cfg(test)]
mod tests;
