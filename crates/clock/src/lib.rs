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

//! The injected wall clock, behind a trait. A program reads the time only through
//! a [`Clock`] it was handed, so a test can run on tokio's paused clock ([`TokioClock`]) or set
//! the time by hand ([`FakeClock`]), and production passes [`SystemClock`], the one wall-clock
//! read.
//!
//! The clock is a wall clock ([`jiff::Timestamp`]), not a monotonic one: it is what a log line or
//! a metric prints. A deadline or a timer belongs on `tokio::time::Instant`.
//!
//! Extracted from swarm-scheduler-exporter/src/clock.rs at c9e79afa8810 (swarm-device-access's
//! copy at 8b4ab3a3e417 differs only in its docs), with `FakeClock` made cloneable and the
//! concurrency test from awtrix-controller/src/clock/mod.rs at afcd9ca26324.

use std::sync::{Arc, Mutex, PoisonError};

use jiff::{SignedDuration, Timestamp};

/// A source of the current time.
pub trait Clock: Send + Sync {
    /// The current time.
    fn now(&self) -> Timestamp;
}

/// A closure is a clock, so a test can hand one in directly.
impl<F> Clock for F
where
    F: Fn() -> Timestamp + Send + Sync,
{
    fn now(&self) -> Timestamp {
        self()
    }
}

/// The system's wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that follows tokio's clock: a base time plus the tokio time elapsed since it was
/// created. Under a paused runtime (`start_paused`) it moves only as tokio's clock does, so the
/// wall time and the timers agree.
///
/// The time saturates at [`Timestamp::MAX`] rather than wrapping or failing. jiff's range ends in
/// the year 9999, so no real run gets there. Both copies this was extracted from saturated there
/// too: they differed only in the fallback of a `saturating_add` that cannot fail (one fell back
/// to the base time, the other to `Timestamp::MAX`), so neither fallback was reachable.
#[derive(Debug, Clone, Copy)]
pub struct TokioClock {
    base: Timestamp,
    start: tokio::time::Instant,
}

impl TokioClock {
    /// A clock reading `base` now and moving with tokio's clock from here on.
    #[must_use]
    pub fn new(base: Timestamp) -> Self {
        Self {
            base,
            start: tokio::time::Instant::now(),
        }
    }
}

impl Clock for TokioClock {
    fn now(&self) -> Timestamp {
        let elapsed = SignedDuration::try_from(self.start.elapsed()).unwrap_or(SignedDuration::MAX);
        // `elapsed` is never negative, so the only overflow is past the end of the range.
        self.base.checked_add(elapsed).unwrap_or(Timestamp::MAX)
    }
}

/// A clock set by hand. Clones share the time: setting or advancing one moves them all, so a test
/// can keep a handle on a clock it gave away (or wrap one in an `Arc`, as before).
#[derive(Debug, Clone)]
pub struct FakeClock {
    now: Arc<Mutex<Timestamp>>,
}

impl FakeClock {
    /// A clock reading `now` until it is changed.
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self {
            now: Arc::new(Mutex::new(now)),
        }
    }

    /// Sets the time.
    pub fn set(&self, now: Timestamp) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
    }

    /// Moves the time by `by`, saturating at the ends of the range.
    pub fn advance(&self, by: SignedDuration) {
        let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        // Adding a SignedDuration saturates; only a span with calendar units could fail.
        *now = now.saturating_add(by).unwrap_or(*now);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn tokio_clock_follows_the_paused_clock() {
        let clock = TokioClock::new(at(1_000));
        assert_eq!(clock.now(), at(1_000));
        tokio::time::sleep(Duration::from_secs(90)).await;
        assert_eq!(clock.now(), at(1_090));
        tokio::time::advance(Duration::from_millis(250)).await;
        assert_eq!(clock.now(), at(1_090) + SignedDuration::from_millis(250));
    }

    #[tokio::test(start_paused = true)]
    async fn tokio_clock_saturates_at_the_end_of_the_range() {
        let clock = TokioClock::new(Timestamp::MAX - SignedDuration::from_secs(1));
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(clock.now(), Timestamp::MAX);
    }

    #[test]
    fn fake_clock_is_set_and_advanced() {
        let clock = FakeClock::new(at(10));
        clock.advance(SignedDuration::from_secs(5));
        assert_eq!(clock.now(), at(15));
        clock.set(at(3));
        assert_eq!(clock.now(), at(3));
        clock.advance(SignedDuration::MIN);
        assert_eq!(clock.now(), Timestamp::MIN);
        clock.set(at(3));
        clock.advance(SignedDuration::MAX);
        assert_eq!(clock.now(), Timestamp::MAX);
    }

    #[test]
    fn fake_clock_clones_share_the_time() {
        let clock = FakeClock::new(at(10));
        let given_away = clock.clone();
        clock.advance(SignedDuration::from_secs(5));
        assert_eq!(given_away.now(), at(15));
        given_away.set(at(1));
        assert_eq!(clock.now(), at(1));
    }

    /// Ported from awtrix-controller's `TestFakeClockConcurrentSafety`.
    #[test]
    fn fake_clock_is_safe_to_share_between_threads() {
        const THREADS: usize = 10;
        const ITERATIONS: i64 = 100;

        let start = at(1_704_067_200);
        let clock = FakeClock::new(start);
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let writer = clock.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..ITERATIONS {
                    writer.advance(SignedDuration::from_millis(1));
                }
            }));
            let reader = clock.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..ITERATIONS {
                    let _ = reader.now();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let threads = i64::try_from(THREADS).unwrap();
        assert_eq!(
            clock.now(),
            start + SignedDuration::from_millis(threads * ITERATIONS)
        );
    }

    #[test]
    fn system_clock_reads_the_present() {
        // After 2026-01-01, and not frozen at the epoch.
        assert!(SystemClock.now() > at(1_767_225_600));
        // Between two reads of jiff's clock, with a second's slack for a clock step.
        let before = Timestamp::now();
        let now = SystemClock.now();
        let after = Timestamp::now();
        let slack = SignedDuration::from_secs(1);
        assert!(now >= before - slack && now <= after + slack);
    }

    #[test]
    fn a_closure_is_a_clock() {
        let clock = || at(42);
        assert_eq!(Clock::now(&clock), at(42));
        let shared: Arc<dyn Clock> = Arc::new(clock);
        assert_eq!(shared.now(), at(42));
    }
}
