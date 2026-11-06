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

//! The suite context ends at its deadline. It watches SIGINT and SIGTERM, which installs
//! process-wide signal handlers, so it runs in a test binary of its own rather than beside the
//! library's unit tests, where those handlers would outlive it.

use std::time::Duration;

use testenv_core::testing::{Runner, RunnerConfig, TallyStyle, TimeoutRule};

/// swarm-scheduler-exporter's runner.
const SSE: RunnerConfig = RunnerConfig {
    prefix: "[it]",
    usage: "usage: integration [--run <pattern>]",
    timeout_env: "SSE_IT_SUITE_TIMEOUT",
    default_timeout: Duration::from_mins(12),
    timeout_rule: TimeoutRule::Clamped,
    deadline_check: false,
    tally: TallyStyle::PassedFailed,
};

/// The context is live before the deadline and ended after it; the signals are watched on the
/// runtime.
#[tokio::test(start_paused = true)]
async fn the_suite_context_ends_at_the_deadline() {
    let runner = Runner::new(SSE);
    let ctx = runner.suite_context(Duration::from_secs(5)).unwrap();
    assert_eq!(ctx.err(), None);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(ctx.err(), None);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(ctx.err().is_some());
}
