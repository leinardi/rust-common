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

//! The connection deadlines at the I/O level, on a paused clock over a duplex: a read waiting
//! under one deadline when the service sets another.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::time::Instant;

use super::*;

const TIMEOUTS: Timeouts = Timeouts {
    read_header: Duration::from_secs(5),
    read: Duration::from_secs(10),
    write: Duration::from_secs(15),
    idle: Duration::from_mins(1),
};

/// A read is pending under the first request's read deadline (10 s) when its response ends with
/// the next request's head already read: that head is due 5 s after the response, at 6 s, and the
/// read waiting since before must fail then, not when its own 10 s timer fires.
#[tokio::test(start_paused = true)]
async fn an_earlier_deadline_set_while_a_read_waits_is_enforced_at_once() {
    let start = Instant::now();
    let (mut client, server) = tokio::io::duplex(1024);
    let state = ConnState::new(TIMEOUTS);
    let mut io = TimedIo::new(server, state.clone());
    let pipelined = b"GET / HTTP/1.1\r\nHost: x\r\n\r\nGET /hea";
    client.write_all(pipelined).await.unwrap();
    let mut buf = [0_u8; 1024];
    assert_eq!(io.read(&mut buf).await.unwrap(), pipelined.len());
    state.on_request_head();
    let reading = tokio::spawn(async move {
        let mut buf = [0_u8; 64];
        let result = io.read(&mut buf).await;
        (result.map_err(|error| error.kind()), start.elapsed())
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    state.on_response_done();
    let (result, elapsed) = reading.await.unwrap();
    assert_eq!(result, Err(io::ErrorKind::TimedOut));
    assert_eq!(elapsed, Duration::from_secs(6));
    drop(client);
}
