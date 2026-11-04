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

//! Go's `context.Context`, as much of it as the harness and the suite use: cancellation and a
//! deadline, inherited by children. Every Docker call runs under a context derived from the
//! suite's, so once the suite deadline passes (or the run is interrupted) blocked calls fail fast
//! and teardown still runs on a context of its own.
//!
//! Extracted from swarm-scheduler-exporter/testenv/src/context.rs at c9e79afa8810 (the same as
//! swarm-device-access/testenv/src/context.rs at 8b4ab3a3e417).

use std::fmt;
use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Why a context ended, in Go's wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The context, or one it derives from, was cancelled.
    Canceled,
    /// Its deadline passed.
    DeadlineExceeded,
}

impl fmt::Display for Ended {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Canceled => "context canceled",
            Self::DeadlineExceeded => "context deadline exceeded",
        })
    }
}

impl std::error::Error for Ended {}

/// A cancellation signal with an optional deadline. Clones share the signal.
#[derive(Debug, Clone)]
pub struct Context {
    token: CancellationToken,
    deadline: Option<Instant>,
}

impl Context {
    /// `context.Background()`: never cancelled, no deadline.
    #[must_use]
    pub fn background() -> Self {
        Self {
            token: CancellationToken::new(),
            deadline: None,
        }
    }

    /// `context.WithTimeout`: a child cancelled with this one, whose deadline is the earlier of
    /// this one's and `timeout` from now.
    #[must_use]
    pub fn with_timeout(&self, timeout: Duration) -> Self {
        let deadline = Instant::now().checked_add(timeout);
        Self {
            token: self.token.child_token(),
            deadline: match (self.deadline, deadline) {
                (Some(parent), Some(own)) => Some(parent.min(own)),
                (parent, own) => parent.or(own),
            },
        }
    }

    /// `context.WithCancel`: a child that [`Context::cancel`] ends without ending this one.
    #[must_use]
    pub fn with_cancel(&self) -> Self {
        Self {
            token: self.token.child_token(),
            deadline: self.deadline,
        }
    }

    /// `context.WithoutCancel`: a context that neither this one's cancellation nor its deadline
    /// reaches, for a teardown that must run after the work it cleans up has been cut short.
    #[must_use]
    pub fn without_cancel(&self) -> Self {
        Self::background()
    }

    /// Cancels this context and every context derived from it.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// The deadline, if any.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Why the context has ended, or `None` while it is live.
    #[must_use]
    pub fn err(&self) -> Option<Ended> {
        if self.token.is_cancelled() {
            return Some(Ended::Canceled);
        }
        self.deadline
            .filter(|&deadline| Instant::now() >= deadline)
            .map(|_| Ended::DeadlineExceeded)
    }

    /// Runs `future` until it completes or the context ends, whichever comes first.
    ///
    /// # Errors
    ///
    /// Why the context ended, if it ended first.
    pub async fn run<F: Future>(&self, future: F) -> Result<F::Output, Ended> {
        let until_cancelled = self.token.run_until_cancelled(future);
        let outcome = match self.deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, until_cancelled)
                .await
                .map_err(|_| Ended::DeadlineExceeded)?,
            None => until_cancelled.await,
        };
        outcome.ok_or(Ended::Canceled)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Context, Ended};

    /// A child ends with its parent, and keeps the earlier deadline; a cancelled child leaves its
    /// parent live; `without_cancel` is reached by neither.
    #[tokio::test(start_paused = true)]
    async fn children_inherit_cancellation_and_the_earlier_deadline() {
        let parent = Context::background().with_timeout(Duration::from_secs(10));
        let child = parent.with_timeout(Duration::from_secs(60));
        assert_eq!(child.deadline(), parent.deadline());
        let shorter = parent.with_timeout(Duration::from_secs(1));
        assert!(shorter.deadline() < parent.deadline());

        let cancelled = parent.with_cancel();
        cancelled.cancel();
        assert_eq!(cancelled.err(), Some(Ended::Canceled));
        assert_eq!(parent.err(), None);

        let detached = parent.without_cancel();
        parent.cancel();
        assert_eq!(child.err(), Some(Ended::Canceled));
        assert_eq!(detached.err(), None);
        assert_eq!(detached.deadline(), None);
        assert_eq!(Ended::Canceled.to_string(), "context canceled");
    }

    /// `run` ends at the deadline or the cancellation, whichever comes first, in Go's words.
    #[tokio::test(start_paused = true)]
    async fn run_ends_with_the_context() {
        let ctx = Context::background().with_timeout(Duration::from_secs(1));
        assert_eq!(ctx.run(async { 7 }).await, Ok(7));
        let ended = ctx.run(std::future::pending::<()>()).await;
        assert_eq!(ended, Err(Ended::DeadlineExceeded));
        assert_eq!(ctx.err(), Some(Ended::DeadlineExceeded));
        assert_eq!(
            Ended::DeadlineExceeded.to_string(),
            "context deadline exceeded"
        );

        let cancelled = Context::background();
        let waiting = cancelled.clone();
        let (ran, ()) = tokio::join!(waiting.run(std::future::pending::<()>()), async {
            cancelled.cancel();
        });
        assert_eq!(ran, Err(Ended::Canceled));
    }
}
