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

//! `context::current` from outside the crate: a consumer's fake engine reads the caller's
//! context the way the crate's own client does.

use std::time::Duration;

use docker_engine::context::{self, ContextError, with_context_timeout};

/// Outside a call there is no context; inside one the fake gets the caller's, deadline included.
#[tokio::test(start_paused = true)]
async fn a_fake_reads_the_callers_context() {
    assert!(context::current().is_none());
    let caller = with_context_timeout(Duration::from_secs(5), async { Ok(context::current()) })
        .await
        .unwrap_or_else(|error| panic!("the call failed: {error}"))
        .unwrap_or_else(|| panic!("no context inside the call"));
    assert_eq!(caller.err(), None);
    tokio::time::advance(Duration::from_secs(6)).await;
    assert_eq!(caller.err(), Some(ContextError::DeadlineExceeded));
}
