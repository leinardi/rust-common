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

//! What the Go suites took from the testing package, for an async `harness = false` runner on a
//! current-thread runtime: a test handle ([`TestHandle`]) with Go's `Errorf` (record and go on),
//! `Fatalf` (record and return [`Fatal`]), `Skipf`, `Logf`, `Cleanup` (last registered, first
//! run, after the test's context is cancelled), `Failed` and `Run` for subtests; the suite's
//! shared state ([`Suite`]); and the runner around them ([`Runner`]): `--run`, the suite
//! deadline, SIGINT and SIGTERM, and the tally. Output follows Go's verbose test output: `=== RUN`,
//! the test's log lines, then `--- PASS:`, `--- FAIL:` or `--- SKIP:` with the seconds, subtests
//! indented under their parent.
//!
//! The output goes to the process's standard error, where the harness's log goes too, as the
//! suites printed it ([`Output::stderr`]); a test of a runner hands it a buffer instead. What each
//! suite prints differently is a [`RunnerConfig`]:
//!
//! - swarm-scheduler-exporter: `SSE_IT_SUITE_TIMEOUT`, 12m, [`TimeoutRule::Clamped`], no
//!   `deadline_check`, [`TallyStyle::PassedFailed`];
//! - swarm-device-access: `SDA_IT_SUITE_TIMEOUT`, 4m, [`TimeoutRule::Positive`], `deadline_check`,
//!   [`TallyStyle::WithSkipped`];
//!
//! both with `prefix` `[it]` and `usage` `usage: integration [--run <pattern>]`. The suite's
//! tests, its state and its setup stay its own.
//!
//! Extracted from swarm-device-access/testenv/tests/integration/testing.rs at 8b4ab3a3e417 (the
//! superset: `Skipf`, [`Outcome`], the deadline checked before a test, the boxed subtests) and
//! swarm-scheduler-exporter/testenv/tests/integration/testing.rs at c9e79afa8810. Extracted from
//! swarm-scheduler-exporter/testenv/tests/integration/main.rs at c9e79afa8810 and
//! swarm-device-access/testenv/tests/integration/main.rs at 8b4ab3a3e417 (`run_pattern`, the
//! selection, the runtime, the suite timeout and context, the tally), for [`Runner`].

use std::cell::{Cell, RefCell};
use std::fmt::Display;
use std::fmt::Write as _;
use std::future::Future;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::OnceLock;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use go_compat::{duration, quote};
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::Instant;

use crate::context::Context;
use crate::run_filter::RunFilter;

/// futures-util's `LocalBoxFuture`: a boxed future that need not be `Send`.
pub type LocalBoxFuture<'future, T> = Pin<Box<dyn Future<Output = T> + 'future>>;

/// A test stopped by a fatal failure or a skip, which the handle has already recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fatal;

/// What a test or subtest returns.
pub type TestResult = Result<(), Fatal>;

/// Turns a failed result into a fatal test failure, its error as the message.
pub trait OrFatal<V> {
    /// The value, or [`Fatal`] with the error recorded on `test`.
    ///
    /// # Errors
    ///
    /// [`Fatal`] when `self` is an error.
    fn or_fatal(self, test: &TestHandle) -> Result<V, Fatal>;
}

impl<V, E: Display> OrFatal<V> for Result<V, E> {
    fn or_fatal(self, test: &TestHandle) -> Result<V, Fatal> {
        self.map_err(|error| test.fatal(error))
    }
}

/// One top-level test, under its Go name.
#[derive(Clone, Copy)]
pub struct Test {
    /// The name `--run` matches, such as `TestNode_Drain`.
    pub name: &'static str,
    /// The body.
    pub run: fn(TestHandle) -> LocalBoxFuture<'static, TestResult>,
}

/// Where the runner and its tests write.
#[derive(Clone)]
pub struct Output(Option<Rc<RefCell<dyn Write>>>);

impl Output {
    /// The process's standard error, locked and flushed for every write.
    #[must_use]
    pub fn stderr() -> Self {
        Self(None)
    }

    /// `writer`, for a test of a runner.
    #[must_use]
    pub fn to(writer: impl Write + 'static) -> Self {
        Self(Some(Rc::new(RefCell::new(writer))))
    }

    /// Writes `text` as it is; a failed write has nowhere left to be reported.
    pub fn write(&self, text: &str) {
        match &self.0 {
            None => {
                let mut stderr = io::stderr().lock();
                let _ = stderr.write_all(text.as_bytes());
                let _ = stderr.flush();
            }
            Some(writer) => {
                let mut writer = writer.borrow_mut();
                let _ = writer.write_all(text.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    /// Writes `line` and a newline.
    pub fn say(&self, line: &str) {
        self.write(&format!("{line}\n"));
    }
}

/// One line to stderr, where the harness's log goes too.
pub fn say(line: &str) {
    Output::stderr().say(line);
}

/// What every test shares, set up once before the first test: the suite's own state, in a
/// static.
pub struct Suite<S> {
    cell: OnceLock<S>,
    unset: &'static str,
}

impl<S> Suite<S> {
    /// An empty suite; [`Suite::get`] before [`Suite::install`] panics with `unset` (such as
    /// `the suite is used before bring-up`).
    #[must_use]
    pub const fn new(unset: &'static str) -> Self {
        Self {
            cell: OnceLock::new(),
            unset,
        }
    }

    /// Installs the suite; the runner calls it once, after the setup.
    ///
    /// # Panics
    ///
    /// The suite is already installed.
    pub fn install(&self, suite: S) {
        assert!(self.cell.set(suite).is_ok(), "the suite is installed once");
    }

    /// The suite.
    ///
    /// # Panics
    ///
    /// The suite is not installed yet, with the message [`Suite::new`] was given.
    pub fn get(&self) -> &S {
        self.cell.get().unwrap_or_else(|| panic!("{}", self.unset))
    }
}

type Cleanup = Box<dyn FnOnce() -> LocalBoxFuture<'static, ()>>;

struct Inner {
    path: Vec<String>,
    ctx: Context,
    filter: Rc<RunFilter>,
    out: Output,
    failed: Cell<bool>,
    skipped: Cell<bool>,
    cleanups: RefCell<Vec<Cleanup>>,
}

/// A running test or subtest. Clones share it.
#[derive(Clone)]
pub struct TestHandle(Rc<Inner>);

impl TestHandle {
    fn new(path: Vec<String>, ctx: Context, filter: Rc<RunFilter>, out: Output) -> Self {
        Self(Rc::new(Inner {
            path,
            ctx,
            filter,
            out,
            failed: Cell::new(false),
            skipped: Cell::new(false),
            cleanups: RefCell::new(Vec::new()),
        }))
    }

    /// The full name: `TestX`, or `TestX/subtest`.
    #[must_use]
    pub fn name(&self) -> String {
        self.0.path.join("/")
    }

    fn indent(&self) -> String {
        "    ".repeat(self.0.path.len().saturating_sub(1))
    }

    /// Go's `testCtx(t)`: ends with the test, or at the suite deadline, whichever comes first.
    #[must_use]
    pub fn ctx(&self) -> &Context {
        &self.0.ctx
    }

    /// `t.Logf`: printed now, under the test's name.
    pub fn log(&self, message: impl Display) {
        let indent = format!("{}    ", self.indent());
        let text = message.to_string();
        let mut lines = text.lines();
        let mut out = String::new();
        if let Some(first) = lines.next() {
            let _ = writeln!(out, "{indent}{first}");
        }
        for line in lines {
            let _ = writeln!(out, "{indent}    {line}");
        }
        self.0.out.write(&out);
    }

    /// `t.Errorf`: records a failure and lets the test go on.
    pub fn error(&self, message: impl Display) {
        self.log(message);
        self.0.failed.set(true);
    }

    /// `t.Fatalf`: records a failure; return the [`Fatal`] to stop the test.
    pub fn fatal(&self, message: impl Display) -> Fatal {
        self.error(message);
        Fatal
    }

    /// `t.Skipf`: logs why, and marks the test skipped unless it has already failed; return the
    /// [`Fatal`] to stop it.
    pub fn skip(&self, message: impl Display) -> Fatal {
        self.log(message);
        self.0.skipped.set(true);
        Fatal
    }

    /// `t.Failed`.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.0.failed.get()
    }

    /// `t.Cleanup`: `cleanup` runs when the test ends, after its context is cancelled, the last
    /// registered first.
    pub fn cleanup<F, Fut>(&self, cleanup: F)
    where
        F: FnOnce() -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        self.0
            .cleanups
            .borrow_mut()
            .push(Box::new(move || Box::pin(cleanup())));
    }

    /// `t.Run`: runs `body` as the subtest `name`, unless the run filter leaves it out, and
    /// returns whether it passed. A failed subtest fails its parent.
    pub async fn run<F, Fut>(&self, name: &str, body: F) -> bool
    where
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = TestResult>,
    {
        let mut path = self.0.path.clone();
        path.push(name.to_owned());
        let names: Vec<&str> = path.iter().map(String::as_str).collect();
        if !self.0.filter.matches(&names) {
            return true;
        }
        let sub = Self::new(
            path,
            self.0.ctx.with_cancel(),
            Rc::clone(&self.0.filter),
            self.0.out.clone(),
        );
        // Boxed, so a test's future does not carry its subtests' inline.
        execute(&sub, Box::pin(body(sub.clone()))).await;
        if sub.failed() {
            self.0.failed.set(true);
        }
        !sub.failed()
    }

    fn outcome(&self) -> Outcome {
        if self.failed() {
            Outcome::Failed
        } else if self.0.skipped.get() {
            Outcome::Skipped
        } else {
            Outcome::Passed
        }
    }
}

/// How a top-level test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It passed.
    Passed,
    /// It skipped itself, and did not fail first.
    Skipped,
    /// It failed.
    Failed,
}

/// A future whose panic is caught, as futures-util's `catch_unwind`.
struct CatchUnwind<F>(Pin<Box<F>>);

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn std::any::Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let inner = self.0.as_mut();
        match catch_unwind(AssertUnwindSafe(|| inner.poll(context))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

/// Runs a test body, a panic counting as a failure, then its cleanups, then prints its result.
async fn execute(test: &TestHandle, body: impl Future<Output = TestResult>) {
    test.0
        .out
        .write(&format!("{}=== RUN   {}\n", test.indent(), test.name()));
    let started = Instant::now();
    if let Err(panic) = CatchUnwind(Box::pin(body)).await {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        test.error(format!("panic: {message}"));
    }
    test.0.ctx.cancel();
    loop {
        let next = test.0.cleanups.borrow_mut().pop();
        let Some(cleanup) = next else {
            break;
        };
        cleanup().await;
    }
    let verdict = match test.outcome() {
        Outcome::Failed => "FAIL",
        Outcome::Skipped => "SKIP",
        Outcome::Passed => "PASS",
    };
    test.0.out.write(&format!(
        "{}--- {verdict}: {} ({:.2}s)\n",
        test.indent(),
        test.name(),
        started.elapsed().as_secs_f64()
    ));
}

/// How a suite reads its deadline variable when it is set and not empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutRule {
    /// Any Go duration, a negative one clamped to zero; one that does not parse fails the run
    /// (exit 1) with `<prefix> parse <VAR>: <error>` (swarm-scheduler-exporter).
    Clamped,
    /// A positive Go duration, as Go's `runSuite` reads it; anything else is a usage error (exit
    /// 2) with `invalid <VAR>=<value, Go-quoted>: want a positive duration`, unprefixed
    /// (swarm-device-access).
    Positive,
}

/// How the run's last line counts the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TallyStyle {
    /// `<prefix> <n> selected, <n - failed> passed, <failed> failed`: a skipped test counts as
    /// passed (swarm-scheduler-exporter).
    PassedFailed,
    /// `<prefix> <n> selected, <passed> passed, <skipped> skipped, <failed> failed`
    /// (swarm-device-access).
    WithSkipped,
}

/// What a suite's runner prints and reads; see the module comment for each suite's values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerConfig {
    /// What every runner line starts with, such as `[it]`.
    pub prefix: &'static str,
    /// The usage line after a command-line error, such as
    /// `usage: integration [--run <pattern>]`.
    pub usage: &'static str,
    /// The variable that sets the suite deadline.
    pub timeout_env: &'static str,
    /// The deadline without it.
    pub default_timeout: Duration,
    /// How the variable is read.
    pub timeout_rule: TimeoutRule,
    /// Whether a test that starts after the deadline fails at once, as Go's `testCtx` does,
    /// with `integration suite deadline exceeded (<VAR>=<value>)`, instead of running its body.
    pub deadline_check: bool,
    /// How the last line counts.
    pub tally: TallyStyle,
}

/// The counts of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// The tests selected.
    pub selected: usize,
    /// The tests that passed.
    pub passed: usize,
    /// The tests that skipped themselves.
    pub skipped: usize,
    /// The tests that failed.
    pub failed: usize,
}

/// A suite's runner: its configuration and where it writes.
#[derive(Clone)]
pub struct Runner {
    config: RunnerConfig,
    out: Output,
}

impl Runner {
    /// A runner writing to standard error.
    #[must_use]
    pub fn new(config: RunnerConfig) -> Self {
        Self::with_output(config, Output::stderr())
    }

    /// A runner writing to `out`.
    #[must_use]
    pub fn with_output(config: RunnerConfig, out: Output) -> Self {
        Self { config, out }
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &RunnerConfig {
        &self.config
    }

    /// One runner line: `line` after the prefix and a space.
    pub fn say(&self, line: &str) {
        self.out.say(&format!("{} {line}", self.config.prefix));
    }

    /// `main`'s first half: the `--run` pattern from `args` (the command line after the program
    /// name), the tests of `tests` it selects, in order, and `<prefix> selected <n> of <m>
    /// tests`.
    ///
    /// # Errors
    ///
    /// The exit code after the message is printed: 2 for a command-line error (with the usage),
    /// 1 for a pattern that does not compile or selects no test.
    pub fn select<'tests>(
        &self,
        args: &[String],
        tests: &'tests [Test],
    ) -> Result<(RunFilter, Vec<&'tests Test>), u8> {
        let pattern = run_pattern(args).map_err(|message| {
            self.say(&format!("{message}\n{}", self.config.usage));
            2_u8
        })?;
        let filter = RunFilter::parse(&pattern).map_err(|error| {
            self.say(&error.to_string());
            1_u8
        })?;
        let names: Vec<&str> = tests.iter().map(|test| test.name).collect();
        let selected = filter.select(&names).map_err(|error| {
            self.say(&error.to_string());
            1_u8
        })?;
        self.say(&format!(
            "selected {} of {} tests",
            selected.len(),
            names.len()
        ));
        let tests = tests
            .iter()
            .filter(|test| selected.contains(&test.name))
            .collect();
        Ok((filter, tests))
    }

    /// The current-thread runtime the suite runs on, with every driver enabled.
    ///
    /// # Errors
    ///
    /// Exit 1 after `<prefix> runtime: <error>`.
    pub fn runtime(&self) -> Result<tokio::runtime::Runtime, u8> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                self.say(&format!("runtime: {error}"));
                1
            })
    }

    /// The suite deadline, from the variable as `getenv` reads it (unset is empty), by the
    /// configured rule.
    ///
    /// # Errors
    ///
    /// The exit code after the message is printed; see [`TimeoutRule`].
    pub fn suite_timeout(&self, getenv: impl Fn(&str) -> Option<String>) -> Result<Duration, u8> {
        let name = self.config.timeout_env;
        let raw = getenv(name).unwrap_or_default();
        if raw.is_empty() {
            return Ok(self.config.default_timeout);
        }
        match self.config.timeout_rule {
            TimeoutRule::Clamped => match duration::parse(&raw) {
                Ok(nanos) => Ok(Duration::from_nanos(nanos.max(0).unsigned_abs())),
                Err(error) => {
                    self.say(&format!("parse {name}: {error}"));
                    Err(1)
                }
            },
            TimeoutRule::Positive => match duration::parse(&raw) {
                Ok(nanos) if nanos > 0 => Ok(Duration::from_nanos(nanos.unsigned_abs())),
                Ok(_) | Err(_) => {
                    self.out.say(&format!(
                        "invalid {name}={}: want a positive duration",
                        quote::quote(&raw)
                    ));
                    Err(2)
                }
            },
        }
    }

    /// The suite context: ends after `timeout`, or on SIGINT or SIGTERM, so the waits return and
    /// the cleanups and teardown run instead of the process being killed mid-test. Must be
    /// called on the runtime.
    ///
    /// # Errors
    ///
    /// Exit 1 after `<prefix> watch SIGINT: <error>` (or `SIGTERM`).
    pub fn suite_context(&self, timeout: Duration) -> Result<Context, u8> {
        let watch = |kind: SignalKind, name: &str| {
            signal(kind).map_err(|error| {
                self.say(&format!("watch {name}: {error}"));
                1_u8
            })
        };
        let ctx = Context::background().with_timeout(timeout);
        let mut interrupt = watch(SignalKind::interrupt(), "SIGINT")?;
        let mut terminate = watch(SignalKind::terminate(), "SIGTERM")?;
        let cancel = ctx.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
            cancel.cancel();
        });
        Ok(ctx)
    }

    /// Runs the top-level test `name` with `body` under a child of `ctx`, its subtests matched
    /// against `filter`, and returns how it ended.
    pub async fn run_test<F, Fut>(
        &self,
        name: &str,
        ctx: &Context,
        filter: &RunFilter,
        body: F,
    ) -> Outcome
    where
        F: FnOnce(TestHandle) -> Fut,
        Fut: Future<Output = TestResult>,
    {
        let test = TestHandle::new(
            vec![name.to_owned()],
            ctx.with_cancel(),
            Rc::new(filter.clone()),
            self.out.clone(),
        );
        // Checked up front, as Go's testCtx does, so a test starting after the deadline fails
        // instead of reaching a wait that would report the dead context as something else.
        if self.config.deadline_check && ctx.err().is_some() {
            let name = self.config.timeout_env;
            let deadline = std::env::var(name).unwrap_or_default();
            let body = async |test: TestHandle| -> TestResult {
                Err(test.fatal(format!(
                    "integration suite deadline exceeded ({name}={deadline})"
                )))
            };
            execute(&test, body(test.clone())).await;
        } else {
            execute(&test, body(test.clone())).await;
        }
        test.outcome()
    }

    /// Runs `tests` one after another under `ctx`, then prints the tally line and returns the
    /// counts.
    pub async fn run_tests(&self, tests: &[&Test], ctx: &Context, filter: &RunFilter) -> Tally {
        let mut tally = Tally {
            selected: tests.len(),
            passed: 0,
            skipped: 0,
            failed: 0,
        };
        for test in tests {
            match self.run_test(test.name, ctx, filter, test.run).await {
                Outcome::Passed => tally.passed += 1,
                Outcome::Skipped => tally.skipped += 1,
                Outcome::Failed => tally.failed += 1,
            }
        }
        self.say(&match self.config.tally {
            TallyStyle::PassedFailed => format!(
                "{} selected, {} passed, {} failed",
                tally.selected,
                tally.selected - tally.failed,
                tally.failed
            ),
            TallyStyle::WithSkipped => format!(
                "{} selected, {} passed, {} skipped, {} failed",
                tally.selected, tally.passed, tally.skipped, tally.failed
            ),
        });
        tally
    }
}

/// The `--run` value: `--run <pattern>` or `--run=<pattern>`, the last one winning as Go's flag
/// package lets it.
///
/// # Errors
///
/// `flag needs an argument: --run`, or `unknown argument "<arg>"` for anything else.
pub fn run_pattern(args: &[String]) -> Result<String, String> {
    let mut pattern = String::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if let Some(value) = arg.strip_prefix("--run=") {
            value.clone_into(&mut pattern);
        } else if arg == "--run" {
            rest.next()
                .ok_or_else(|| "flag needs an argument: --run".to_owned())?
                .clone_into(&mut pattern);
        } else {
            return Err(format!("unknown argument {arg:?}"));
        }
    }
    Ok(pattern)
}

#[cfg(test)]
mod tests;
