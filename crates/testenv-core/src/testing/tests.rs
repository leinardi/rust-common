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

//! The async runner's output, line for line as the suites print it, on a paused clock so every
//! duration reads `0.00s`.

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::time::Duration;

use super::{
    Fatal, LocalBoxFuture, OrFatal as _, Outcome, Output, Runner, RunnerConfig, Suite, Tally,
    TallyStyle, Test, TestHandle, TestResult, TimeoutRule, run_pattern,
};
use crate::context::Context;
use crate::run_filter::RunFilter;

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

/// swarm-device-access's runner.
const SDA: RunnerConfig = RunnerConfig {
    prefix: "[it]",
    usage: "usage: integration [--run <pattern>]",
    timeout_env: "SDA_IT_SUITE_TIMEOUT",
    default_timeout: Duration::from_mins(4),
    timeout_rule: TimeoutRule::Positive,
    deadline_check: true,
    tally: TallyStyle::WithSkipped,
};

#[derive(Clone, Default)]
struct Buffer(Rc<RefCell<Vec<u8>>>);

impl Buffer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.borrow()).into_owned()
    }
}

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn make_runner(config: RunnerConfig) -> (Runner, Buffer) {
    let buffer = Buffer::default();
    (
        Runner::with_output(config, Output::to(buffer.clone())),
        buffer,
    )
}

fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

async fn passes(test: TestHandle) -> TestResult {
    test.log("first line\nsecond line");
    let ok = test
        .run("sub", async |sub: TestHandle| -> TestResult {
            sub.log("in the subtest");
            Ok(())
        })
        .await;
    assert!(ok);
    Ok(())
}

async fn fails(test: TestHandle) -> TestResult {
    let journal = Rc::new(RefCell::new(Vec::new()));
    for name in ["first", "second"] {
        let journal = Rc::clone(&journal);
        let ctx = test.ctx().clone();
        test.cleanup(move || async move {
            // The test's context is cancelled before its cleanups run.
            journal.borrow_mut().push(format!("{name} {:?}", ctx.err()));
        });
    }
    let journal_check = Rc::clone(&journal);
    test.cleanup(move || async move {
        assert!(
            journal_check.borrow().is_empty(),
            "registered last, run first"
        );
    });
    let passed = test
        .run("bad", async |sub: TestHandle| -> TestResult {
            Err::<(), _>("boom").or_fatal(&sub)?;
            sub.log("not reached");
            Ok(())
        })
        .await;
    assert!(!passed);
    test.error("after the subtest");
    Ok(())
}

const TESTS: &[Test] = &[
    Test {
        name: "TestPasses",
        run: |test| Box::pin(passes(test)) as LocalBoxFuture<'static, TestResult>,
    },
    Test {
        name: "TestFails",
        run: |test| Box::pin(fails(test)),
    },
    Test {
        name: "TestSkips",
        run: |test| Box::pin(async move { Err(test.skip("needs SDA_IT_ENFORCE=1")) }),
    },
    Test {
        name: "TestPanics",
        run: |_| Box::pin(async { panic!("a panic in the body") }),
    },
];

/// The whole run of the four tests, as swarm-device-access prints it, then the tally as
/// swarm-scheduler-exporter counts it.
#[tokio::test(start_paused = true)]
async fn a_run_prints_go_s_verbose_output() {
    let (runner, buffer) = make_runner(SDA);
    let (filter, tests) = runner.select(&[], TESTS).unwrap();
    let ctx = Context::background().with_timeout(Duration::from_mins(4));
    let tally = runner.run_tests(&tests, &ctx, &filter).await;
    assert_eq!(
        tally,
        Tally {
            selected: 4,
            passed: 1,
            skipped: 1,
            failed: 2,
        }
    );
    assert_eq!(
        buffer.text(),
        "[it] selected 4 of 4 tests\n\
         === RUN   TestPasses\n    \
         first line\n        \
         second line\n    \
         === RUN   TestPasses/sub\n        \
         in the subtest\n    \
         --- PASS: TestPasses/sub (0.00s)\n\
         --- PASS: TestPasses (0.00s)\n\
         === RUN   TestFails\n    \
         === RUN   TestFails/bad\n        \
         boom\n    \
         --- FAIL: TestFails/bad (0.00s)\n    \
         after the subtest\n\
         --- FAIL: TestFails (0.00s)\n\
         === RUN   TestSkips\n    \
         needs SDA_IT_ENFORCE=1\n\
         --- SKIP: TestSkips (0.00s)\n\
         === RUN   TestPanics\n    \
         panic: a panic in the body\n\
         --- FAIL: TestPanics (0.00s)\n\
         [it] 4 selected, 1 passed, 1 skipped, 2 failed\n"
    );

    let (runner, buffer) = make_runner(SSE);
    runner.run_tests(&tests, &ctx, &filter).await;
    assert!(
        buffer
            .text()
            .ends_with("\n[it] 4 selected, 2 passed, 2 failed\n"),
        "{}",
        buffer.text()
    );
}

/// Cleanups run last registered first, after the test's context is cancelled.
#[tokio::test(start_paused = true)]
async fn cleanups_run_in_reverse_after_the_cancel() {
    let (runner, _) = make_runner(SSE);
    let journal = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&journal);
    let filter = RunFilter::parse("").unwrap();
    let outcome = runner
        .run_test(
            "TestCleanups",
            &Context::background(),
            &filter,
            async move |test: TestHandle| {
                for name in ["first", "second"] {
                    let journal = Rc::clone(&seen);
                    let ctx = test.ctx().clone();
                    test.cleanup(move || async move {
                        journal.borrow_mut().push(format!("{name} {:?}", ctx.err()));
                    });
                }
                Ok(())
            },
        )
        .await;
    assert_eq!(outcome, Outcome::Passed);
    assert_eq!(
        *journal.borrow(),
        ["second Some(Canceled)", "first Some(Canceled)"]
    );
}

/// The filter reaches the subtests: one it leaves out does not run and counts as passed.
#[tokio::test(start_paused = true)]
async fn the_filter_selects_subtests() {
    let (runner, buffer) = make_runner(SSE);
    let filter = RunFilter::parse("^TestA$/^kept$").unwrap();
    let outcome = runner
        .run_test(
            "TestA",
            &Context::background(),
            &filter,
            async |test: TestHandle| {
                assert!(
                    test.run("dropped", async |_| -> TestResult { Err(Fatal) })
                        .await
                );
                assert!(test.run("kept", async |_| -> TestResult { Ok(()) }).await);
                Ok(())
            },
        )
        .await;
    assert_eq!(outcome, Outcome::Passed);
    let text = buffer.text();
    assert!(!text.contains("dropped"), "{text}");
    assert!(
        text.contains("    --- PASS: TestA/kept (0.00s)\n"),
        "{text}"
    );
}

/// swarm-device-access fails a test that starts after the deadline without running its body;
/// swarm-scheduler-exporter runs it.
#[tokio::test(start_paused = true)]
async fn a_test_after_the_deadline() {
    let ended = Context::background().with_timeout(Duration::ZERO);
    tokio::time::sleep(Duration::from_millis(1)).await;
    let filter = RunFilter::parse("").unwrap();
    let ran = Rc::new(RefCell::new(false));

    let (runner, buffer) = make_runner(SDA);
    let flag = Rc::clone(&ran);
    let outcome = runner
        .run_test(
            "TestLate",
            &ended,
            &filter,
            async move |_: TestHandle| -> TestResult {
                *flag.borrow_mut() = true;
                Ok(())
            },
        )
        .await;
    assert_eq!(outcome, Outcome::Failed);
    assert!(!*ran.borrow());
    assert!(
        buffer
            .text()
            .contains("    integration suite deadline exceeded (SDA_IT_SUITE_TIMEOUT="),
        "{}",
        buffer.text()
    );

    let (runner, _) = make_runner(SSE);
    let flag = Rc::clone(&ran);
    let outcome = runner
        .run_test(
            "TestLate",
            &ended,
            &filter,
            async move |_: TestHandle| -> TestResult {
                *flag.borrow_mut() = true;
                Ok(())
            },
        )
        .await;
    assert_eq!(outcome, Outcome::Passed);
    assert!(*ran.borrow());
}

/// The command line: `--run`, `--run=`, the last winning, and the errors.
#[test]
fn run_patterns() {
    assert_eq!(run_pattern(&[]), Ok(String::new()));
    assert_eq!(
        run_pattern(&args(&["--run", "^A$", "--run=^B$"])),
        Ok("^B$".to_owned())
    );
    assert_eq!(
        run_pattern(&args(&["--run"])),
        Err("flag needs an argument: --run".to_owned())
    );
    assert_eq!(
        run_pattern(&args(&["-v"])),
        Err(r#"unknown argument "-v""#.to_owned())
    );
}

/// The selection's messages and exit codes.
#[test]
fn selection_errors() {
    let (runner, buffer) = make_runner(SSE);
    assert_eq!(runner.select(&args(&["-v"]), TESTS).err(), Some(2));
    assert_eq!(
        buffer.text(),
        "[it] unknown argument \"-v\"\nusage: integration [--run <pattern>]\n"
    );

    let (runner, buffer) = make_runner(SSE);
    assert_eq!(
        runner.select(&args(&["--run", "^TestTypo"]), TESTS).err(),
        Some(1)
    );
    assert_eq!(buffer.text(), "[it] --run \"^TestTypo\" selects no test\n");

    let (runner, buffer) = make_runner(SSE);
    assert_eq!(runner.select(&args(&["--run", "("]), TESTS).err(), Some(1));
    assert!(
        buffer
            .text()
            .starts_with("[it] invalid regexp for element 0 of ("),
        "{}",
        buffer.text()
    );

    let (runner, buffer) = make_runner(SSE);
    let (_, selected) = runner.select(&args(&["--run=Pass|Skip"]), TESTS).unwrap();
    assert_eq!(
        selected.iter().map(|test| test.name).collect::<Vec<_>>(),
        ["TestPasses", "TestSkips"]
    );
    assert_eq!(buffer.text(), "[it] selected 2 of 4 tests\n");
}

/// Each suite's reading of its deadline variable.
#[test]
fn suite_timeouts() {
    let with = |value: &'static str| move |_: &str| Some(value.to_owned());

    let (runner, buffer) = make_runner(SSE);
    assert_eq!(runner.suite_timeout(|_| None), Ok(Duration::from_mins(12)));
    assert_eq!(runner.suite_timeout(with("")), Ok(Duration::from_mins(12)));
    assert_eq!(
        runner.suite_timeout(with("90s")),
        Ok(Duration::from_secs(90))
    );
    assert_eq!(runner.suite_timeout(with("-1s")), Ok(Duration::ZERO));
    assert_eq!(runner.suite_timeout(with("soon")), Err(1));
    assert_eq!(
        buffer.text(),
        "[it] parse SSE_IT_SUITE_TIMEOUT: time: invalid duration \"soon\"\n"
    );

    let (runner, buffer) = make_runner(SDA);
    assert_eq!(runner.suite_timeout(|_| None), Ok(Duration::from_mins(4)));
    assert_eq!(
        runner.suite_timeout(with("90s")),
        Ok(Duration::from_secs(90))
    );
    assert_eq!(runner.suite_timeout(with("0s")), Err(2));
    assert_eq!(runner.suite_timeout(with("soon")), Err(2));
    assert_eq!(
        buffer.text(),
        "invalid SDA_IT_SUITE_TIMEOUT=\"0s\": want a positive duration\n\
         invalid SDA_IT_SUITE_TIMEOUT=\"soon\": want a positive duration\n"
    );
}

/// The suite context ends at its deadline; the signals are watched on the runtime.
#[tokio::test(start_paused = true)]
async fn the_suite_context_ends_at_the_deadline() {
    let (runner, _) = make_runner(SSE);
    let ctx = runner.suite_context(Duration::from_secs(5)).unwrap();
    assert_eq!(ctx.err(), None);
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(ctx.err().is_some());
}

static STATE: Suite<u32> = Suite::new("the suite is used before bring-up");

/// The suite is installed once and read after; before, the message is the suite's own.
#[test]
fn the_suite_is_installed_once() {
    let early = std::panic::catch_unwind(|| *STATE.get());
    let message = early.unwrap_err();
    assert_eq!(
        message.downcast_ref::<String>().map(String::as_str),
        Some("the suite is used before bring-up")
    );
    STATE.install(7);
    assert_eq!(*STATE.get(), 7);
    assert!(std::panic::catch_unwind(|| STATE.install(8)).is_err());
    assert_eq!(*STATE.get(), 7);
}
