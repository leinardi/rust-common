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

//! The decoding of the wait answer, as moby's wait goroutine reads it. swarm-device-access checks
//! its requests on the wire in its own `tests/launcher_wire.rs`.
//!
//! Extracted from swarm-device-access/src/launcher/engine/tests.rs at 8b4ab3a3e417.

use super::{Decoded, WaitExitError, WaitResponse, decode_wait};

/// A complete answer decodes at once, without waiting for the end of the body.
#[test]
fn decode_wait_answer() {
    match decode_wait::<WaitResponse>(
        b"{\"StatusCode\":3,\"Error\":{\"Message\":\"oops\"}}\ntrailing",
    ) {
        Decoded::Value(response) => assert_eq!(
            response,
            WaitResponse {
                status_code: 3,
                error: Some(WaitExitError {
                    message: "oops".to_owned(),
                }),
            }
        ),
        Decoded::Incomplete | Decoded::Syntax | Decoded::Other(_) => panic!("not decoded"),
    }
    assert!(matches!(
        decode_wait::<WaitResponse>(b"{\"StatusCode\":null,\"Error\":null}"),
        Decoded::Value(WaitResponse {
            status_code: 0,
            error: None
        })
    ));
}

/// An answer cut short waits for more; one that is not JSON is a syntax error, reported by its
/// text; a wrong type is the decoder's error.
#[test]
fn decode_wait_failures() {
    assert!(matches!(
        decode_wait::<WaitResponse>(b""),
        Decoded::Incomplete
    ));
    assert!(matches!(
        decode_wait::<WaitResponse>(b"{\"StatusCo"),
        Decoded::Incomplete
    ));
    assert!(matches!(
        decode_wait::<WaitResponse>(b"proxy error"),
        Decoded::Syntax
    ));
    // Q10: serde's wording, where Go has encoding/json's.
    assert!(matches!(
        decode_wait::<WaitResponse>(b"{\"StatusCode\":\"x\"}"),
        Decoded::Other(text)
            if text == "invalid type: string \"x\", expected i64 at line 1 column 17"
    ));
}
