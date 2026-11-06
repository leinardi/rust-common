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

//! What the consumers' Docker tests share (`test-util`): a minimal HTTP/1.1 fake engine
//! ([`fake_engine`]) whose answers are the test's, the normalisation of a recorded request
//! ([`recorder`]), and three helpers: [`OrPanic`] for the helpers of an integration test, a
//! [`TempDir`] removed on drop, and a bounded [`eventually`].
//!
//! Extracted from swarm-scheduler-exporter/tests/support/mod.rs at c9e79afa8810 (`OrPanic`,
//! byte-identical in swarm-device-access/tests/support/mod.rs at 8b4ab3a3e417).
//! Extracted from swarm-device-access/src/test_support.rs at 8b4ab3a3e417 (`TempDir`,
//! `eventually`).

pub mod fake_engine;
pub mod recorder;

use std::fmt::{Debug, Display};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// `unwrap` and `unwrap_err` for the helpers of a test, which clippy's test allowance does not
/// cover: the panic names the caller's line and the error.
pub trait OrPanic<T, E> {
    /// The value, or a panic with the error.
    fn or_panic(self) -> T;
    /// The error, or a panic with the value.
    fn or_panic_err(self) -> E;
}

impl<T: Debug, E: Display> OrPanic<T, E> for Result<T, E> {
    #[track_caller]
    fn or_panic(self) -> T {
        match self {
            Ok(value) => value,
            Err(error) => panic!("unexpected error: {error}"),
        }
    }

    #[track_caller]
    fn or_panic_err(self) -> E {
        match self {
            Ok(value) => panic!("unexpected success: {value:?}"),
            Err(error) => error,
        }
    }
}

/// A temporary directory, removed with what it holds on drop.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// A new, empty directory under the system's temporary directory, named after `prefix`, the
    /// process and a counter.
    ///
    /// # Panics
    ///
    /// When the directory cannot be created.
    #[must_use]
    #[track_caller]
    pub fn new(prefix: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).or_panic();
        Self(path)
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The directory as text.
    ///
    /// # Panics
    ///
    /// When the path is not UTF-8, which a temporary directory's is unless `TMPDIR` says
    /// otherwise.
    #[must_use]
    #[track_caller]
    pub fn str(&self) -> &str {
        match self.0.to_str() {
            Some(text) => text,
            None => panic!("not UTF-8: {}", self.0.display()),
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How often [`eventually`] checks its condition.
const EVENTUALLY_INTERVAL: Duration = Duration::from_millis(10);

/// Waits until `condition` holds, checking every 10ms of tokio time, and fails the test once
/// `within` has passed: a wait for something that never happens fails instead of hanging, also
/// under a paused clock that keeps advancing.
///
/// # Panics
///
/// When `within` passes first, naming `what`.
pub async fn eventually(what: &str, within: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {within:?} waiting for {what}"
        );
        tokio::time::sleep(EVENTUALLY_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests;
