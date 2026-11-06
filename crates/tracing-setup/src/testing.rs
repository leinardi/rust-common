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

//! What a consumer's tests need around the subscriber: [`Capture`], a writer that keeps what was
//! logged, and [`set_default`], a scoped default that sees every record.
//!
//! Extracted from `bitgauge/src/test_support.rs` at f9bb91eb5a50 (`Capture`, `json_lines`).
//! Extracted from `awtrix-controller/src/test_support.rs` at afcd9ca26324 (`Captured::install`, the
//! interest-cache guard).

use std::io;
use std::sync::{Arc, Mutex, Once, PoisonError};

use tracing::Dispatch;
use tracing::dispatcher::DefaultGuard;
use tracing_subscriber::fmt::MakeWriter;

/// A writer that keeps everything written to it; its clones share the buffer, so a test keeps one
/// and hands the other to [`build`](crate::build).
#[derive(Debug, Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// The bytes written so far.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Everything written so far, as text.
    ///
    /// # Panics
    ///
    /// What was written is not UTF-8, which the subscriber never writes.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8(self.bytes())
            .unwrap_or_else(|error| panic!("the log output is not UTF-8: {error}"))
    }

    /// The written lines, each parsed as a JSON object.
    ///
    /// # Panics
    ///
    /// A line is not JSON, naming it.
    #[must_use]
    pub fn json_lines(&self) -> Vec<serde_json::Value> {
        self.text()
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("not a JSON log line: {line:?}: {error}"))
            })
            .collect()
    }
}

impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for Capture {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

/// Makes `dispatch` this thread's default until the guard is dropped, without losing a record to
/// tracing's interest cache. A current-thread tokio test sees the records of the tasks it spawns
/// too.
///
/// tracing caches each callsite's interest across all live dispatchers. A callsite another test
/// thread registers while this default is being installed can cache "never" from a snapshot
/// without it, and this thread then silently loses the record. So the first call also installs a
/// global default that is interested in everything (a bare registry, which writes nothing) when
/// none is installed: it is in every later snapshot, so no shared callsite caches "never"; the
/// rebuild heals one cached before. Use it in a test binary only, where no other global default
/// is wanted.
///
/// The race this guards against is not reproducible on demand, so no test here proves the guard;
/// the scoping itself is tested.
pub fn set_default(dispatch: &Dispatch) -> DefaultGuard {
    static GLOBAL: Once = Once::new();
    GLOBAL.call_once(|| {
        // Fails only when a global default already exists, which then serves the same purpose.
        let _ =
            tracing::dispatcher::set_global_default(Dispatch::new(tracing_subscriber::registry()));
    });
    let guard = tracing::dispatcher::set_default(dispatch);
    tracing::callsite::rebuild_interest_cache();
    guard
}
