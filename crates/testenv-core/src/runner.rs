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

//! The runner of the Docker suites, the `harness = false` test targets: what they took from Go's
//! `testing` package, without async.
//!
//! A suite is a list of named [`TestCase`]s and a setup step. [`main`] parses the command line,
//! selects the tests, runs the setup once before any of them (where the image identity check
//! goes), then runs the selected tests one after another and exits 1 on any
//! failure. Tests do not run in parallel: they share the daemon, and a sequential log reads.
//!
//! Each test gets a [`Test`] handle with Go's `Errorf` ([`Test::error`], record and go on),
//! `Fatalf` ([`Test::fatal`], record and return [`Fatal`]), `Logf`, `Cleanup` (last registered,
//! first run, each told whether its test failed), `Failed`, `TempDir` and `Run` for subtests. A
//! panic in a test body or a cleanup is a failure. Output follows Go's verbose test output on
//! stderr: `=== RUN`, the test's log lines, then `--- PASS:` or `--- FAIL:` with the seconds,
//! subtests indented under their parent.
//!
//! # Command line
//!
//! - `--run <pattern>` (or `--run=<pattern>`) selects tests with Go's `-run` semantics
//!   ([`RunFilter`]), subtests included; a pattern that selects no top-level test fails the run.
//! - The flags cargo and libtest users pass are accepted: `--nocapture`, `--show-output`,
//!   `--quiet`, `--test-threads`, `--color`, `--format` and the like are ignored; `--list` prints
//!   the selected tests as libtest does; `--ignored` runs nothing, since no test here is ignored;
//!   a positional filter selects tests whose name contains it (`--exact`: equals it), and
//!   `--skip` drops them, as in libtest. A libtest filter that leaves nothing runs nothing and
//!   passes, as libtest does, so `cargo test <filter>` across the workspace still works; only
//!   `--run` is held to selecting a test.
//! - Anything else is a usage error, exit 2.
//!
//! The suite names the prefix of its tests' temporary directories (bitgauge's is
//! `bitgauge-test-`), the first argument of [`main`], [`main_selected`], [`run`] and
//! [`run_selected`].
//!
//! Extracted from bitgauge/testenv/src/runner.rs at 600fc3905200 (itself adapted from
//! swarm-scheduler-exporter's async runner), its output unchanged; the temporary directories'
//! prefix, `bitgauge-test-` there, made a parameter.

use std::env;
use std::fmt::{Display, Write as _};
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crate::run_filter::RunFilter;

/// A test stopped by a fatal failure, which [`Test::fatal`] has already recorded.
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
    fn or_fatal(self, test: &Test) -> Result<V, Fatal>;
}

impl<V, E: Display> OrFatal<V> for Result<V, E> {
    fn or_fatal(self, test: &Test) -> Result<V, Fatal> {
        self.map_err(|error| test.fatal(error))
    }
}

/// One top-level test, under its Go name.
pub struct TestCase<S> {
    /// The name `--run` matches, such as `TestImage`.
    pub name: &'static str,
    /// The body, given the test handle and what the setup returned.
    pub run: fn(&Test, &S) -> TestResult,
}

type Sink = Arc<Mutex<Box<dyn Write + Send>>>;
type Cleanup = Box<dyn FnOnce(&Test) + Send>;

struct Inner {
    path: Vec<String>,
    temp_prefix: &'static str,
    filter: Arc<RunFilter>,
    out: Sink,
    failed: AtomicBool,
    cleanups: Mutex<Vec<Cleanup>>,
}

/// A running test or subtest. Clones share it, so a test may hand it to a thread of its own.
#[derive(Clone)]
pub struct Test(Arc<Inner>);

impl Test {
    fn new(
        path: Vec<String>,
        temp_prefix: &'static str,
        filter: Arc<RunFilter>,
        out: Sink,
    ) -> Self {
        Self(Arc::new(Inner {
            path,
            temp_prefix,
            filter,
            out,
            failed: AtomicBool::new(false),
            cleanups: Mutex::new(Vec::new()),
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

    fn say(&self, text: &str) {
        let mut out = self.0.out.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    /// Go's `t.Logf`: printed now, under the test's name.
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
        self.say(&out);
    }

    /// Go's `t.Errorf`: records a failure and lets the test go on.
    pub fn error(&self, message: impl Display) {
        self.log(message);
        self.0.failed.store(true, Ordering::SeqCst);
    }

    /// Go's `t.Fatalf`: records a failure; return the [`Fatal`] to stop the test.
    pub fn fatal(&self, message: impl Display) -> Fatal {
        self.error(message);
        Fatal
    }

    /// Go's `t.Failed`: whether the test, or one of its subtests, has failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.0.failed.load(Ordering::SeqCst)
    }

    /// Go's `t.Cleanup`: `cleanup` runs when the test ends, after its body and its subtests, the
    /// last registered first. It is given the test, so it can ask [`Test::failed`] and report its
    /// own failure with [`Test::error`].
    pub fn cleanup(&self, cleanup: impl FnOnce(&Self) + Send + 'static) {
        self.0
            .cleanups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Box::new(cleanup));
    }

    /// Go's `t.TempDir`: a new directory, named with the suite's prefix, removed when the test
    /// ends.
    ///
    /// # Errors
    ///
    /// [`Fatal`], recorded, when the directory cannot be created.
    pub fn temp_dir(&self) -> Result<PathBuf, Fatal> {
        let dir = tempfile::Builder::new()
            .prefix(self.0.temp_prefix)
            .tempdir()
            .map_err(|error| self.fatal(format!("create a temporary directory: {error}")))?;
        let path = dir.path().to_path_buf();
        self.cleanup(move |_| drop(dir));
        Ok(path)
    }

    /// Go's `t.Run`: runs `body` as the subtest `name`, unless the run filter leaves it out, and
    /// returns whether it passed. Spaces in the name become underscores, as in Go, and the filter
    /// matches that form. A failed subtest fails its parent.
    pub fn run(&self, name: &str, body: impl FnOnce(&Self) -> TestResult) -> bool {
        let mut path = self.0.path.clone();
        path.push(name.replace(' ', "_"));
        let names: Vec<&str> = path.iter().map(String::as_str).collect();
        if !self.0.filter.matches(&names) {
            return true;
        }
        let sub = Self::new(
            path,
            self.0.temp_prefix,
            Arc::clone(&self.0.filter),
            Arc::clone(&self.0.out),
        );
        execute(&sub, body);
        if sub.failed() {
            self.0.failed.store(true, Ordering::SeqCst);
        }
        !sub.failed()
    }
}

/// Runs a test body, a panic counting as a failure, then its cleanups, then prints its result.
fn execute(test: &Test, body: impl FnOnce(&Test) -> TestResult) {
    test.say(&format!("{}=== RUN   {}\n", test.indent(), test.name()));
    let started = Instant::now();
    if let Err(panic) = panic::catch_unwind(AssertUnwindSafe(|| body(test))) {
        test.error(format!("panic: {}", panic_message(panic.as_ref())));
    }
    loop {
        let next = test
            .0
            .cleanups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let Some(cleanup) = next else {
            break;
        };
        if let Err(panic) = panic::catch_unwind(AssertUnwindSafe(|| cleanup(test))) {
            test.error(format!(
                "panic in a cleanup: {}",
                panic_message(panic.as_ref())
            ));
        }
    }
    let verdict = if test.failed() { "FAIL" } else { "PASS" };
    test.say(&format!(
        "{}--- {verdict}: {} ({:.2}s)\n",
        test.indent(),
        test.name(),
        started.elapsed().as_secs_f64()
    ));
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
        .unwrap_or_default()
}

/// The parsed command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    run: String,
    list: bool,
    ignored: bool,
    exact: bool,
    filters: Vec<String>,
    skip: Vec<String>,
}

/// libtest's flags that change nothing here.
const IGNORED_FLAGS: &[&str] = &[
    "--nocapture",
    "--show-output",
    "--quiet",
    "-q",
    "--include-ignored",
    "--report-time",
    "--ensure-time",
    "--shuffle",
    "--exclude-should-panic",
    "--force-run-in-process",
    "--test",
];

/// libtest's flags that take a value and change nothing here.
const IGNORED_VALUE_FLAGS: &[&str] = &[
    "--test-threads",
    "--color",
    "--format",
    "--logfile",
    "--shuffle-seed",
    "-Z",
];

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value.to_owned())),
            Some(_) | None => (arg.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| rest.next().cloned())
                .ok_or_else(|| format!("flag needs an argument: {flag}"))
        };
        match flag {
            "--run" => parsed.run = value()?,
            "--skip" => parsed.skip.push(value()?),
            "--list" => parsed.list = true,
            "--ignored" => parsed.ignored = true,
            "--exact" => parsed.exact = true,
            _ if IGNORED_FLAGS.contains(&flag) && inline.is_none() => {}
            _ if IGNORED_VALUE_FLAGS.contains(&flag) => {
                value()?;
            }
            _ if flag.starts_with('-') => return Err(format!("unknown argument {arg:?}")),
            _ => parsed.filters.push(arg.clone()),
        }
    }
    Ok(parsed)
}

/// Whether libtest's positional filters and `--skip` keep `name`.
fn libtest_keeps(args: &Args, name: &str) -> bool {
    let matches = |filter: &String| {
        if args.exact {
            name == filter
        } else {
            name.contains(filter.as_str())
        }
    };
    (args.filters.is_empty() || args.filters.iter().any(matches)) && !args.skip.iter().any(matches)
}

/// Runs the suite on this process's command line and exits as [`run`] says; `temp_prefix` starts
/// the name of every [`Test::temp_dir`].
pub fn main<S>(
    temp_prefix: &'static str,
    tests: &[TestCase<S>],
    setup: impl FnOnce() -> Result<S, String>,
) -> ExitCode {
    main_selected(temp_prefix, tests, |_| setup())
}

/// [`main`] whose setup is told the names of the tests about to run, for a suite whose
/// tests need different preparation (bitgauge's harness checks its image only for the runs that
/// start it).
pub fn main_selected<S>(
    temp_prefix: &'static str,
    tests: &[TestCase<S>],
    setup: impl FnOnce(&[&'static str]) -> Result<S, String>,
) -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    ExitCode::from(run_selected(temp_prefix, &args, tests, setup, io::stderr()))
}

/// Runs the suite: selects the tests by `args`, runs `setup` once before the first of them,
/// runs them in order with the setup's value, drops that value (its teardown), and prints the
/// counts to `out`; `temp_prefix` starts the name of every [`Test::temp_dir`]. Returns the exit
/// status: 0 when every selected test passed (or nothing was
/// selected by a libtest filter), 1 on a failure, a failed setup or a `--run` pattern that
/// selects nothing, 2 for a usage error.
pub fn run<S>(
    temp_prefix: &'static str,
    args: &[String],
    tests: &[TestCase<S>],
    setup: impl FnOnce() -> Result<S, String>,
    out: impl Write + Send + 'static,
) -> u8 {
    run_selected(temp_prefix, args, tests, |_| setup(), out)
}

/// [`run`] whose setup is given the names of the selected tests.
pub fn run_selected<S>(
    temp_prefix: &'static str,
    args: &[String],
    tests: &[TestCase<S>],
    setup: impl FnOnce(&[&'static str]) -> Result<S, String>,
    out: impl Write + Send + 'static,
) -> u8 {
    let out: Sink = Arc::new(Mutex::new(Box::new(out)));
    let say = |line: &str| {
        let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    };
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => {
            say(&format!(
                "{message}\nusage: [--run <pattern>] [libtest flags and filters]"
            ));
            return 2;
        }
    };
    let filter = match RunFilter::parse(&args.run) {
        Ok(filter) => Arc::new(filter),
        Err(error) => {
            say(&error.to_string());
            return 1;
        }
    };
    if args.ignored {
        // No test here is ignored, so libtest's --ignored selects none.
        if !args.list {
            say("no test is ignored; --ignored runs none");
        }
        return 0;
    }
    let names: Vec<&str> = tests.iter().map(|test| test.name).collect();
    let selected = match filter.select(&names) {
        Ok(selected) => selected,
        Err(error) => {
            say(&error.to_string());
            return 1;
        }
    };
    let selected: Vec<&TestCase<S>> = tests
        .iter()
        .filter(|test| selected.contains(&test.name) && libtest_keeps(&args, test.name))
        .collect();
    if args.list {
        for test in &selected {
            say(&format!("{}: test", test.name));
        }
        return 0;
    }
    if selected.is_empty() {
        say("the libtest filters select no test; nothing to run");
        return 0;
    }
    say(&format!(
        "selected {} of {} tests",
        selected.len(),
        tests.len()
    ));
    let names: Vec<&'static str> = selected.iter().map(|test| test.name).collect();
    let state = match setup(&names) {
        Ok(state) => state,
        Err(message) => {
            say(&format!("setup failed: {message}"));
            say("FAIL");
            return 1;
        }
    };
    let mut failed = Vec::new();
    for test in &selected {
        let handle = Test::new(
            vec![test.name.to_owned()],
            temp_prefix,
            Arc::clone(&filter),
            Arc::clone(&out),
        );
        execute(&handle, |handle| (test.run)(handle, &state));
        if handle.failed() {
            failed.push(test.name);
        }
    }
    drop(state);
    say(&format!(
        "{} passed, {} failed{}",
        selected.len() - failed.len(),
        failed.len(),
        if failed.is_empty() {
            String::new()
        } else {
            format!(": {}", failed.join(", "))
        }
    ));
    if failed.is_empty() {
        say("PASS");
        0
    } else {
        say("FAIL");
        1
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use std::io::{self, Write};

    use super::{Args, Fatal, OrFatal, TestCase, TestResult, parse_args, run};

    /// What the runner wrote.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    type Journal = Arc<Mutex<Vec<String>>>;

    fn note(journal: &Journal, entry: impl Into<String>) {
        journal.lock().unwrap().push(entry.into());
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "a TestCase body returns a TestResult"
    )]
    fn passes(test: &super::Test, journal: &Journal) -> TestResult {
        note(journal, test.name());
        let journal_for_cleanup = Arc::clone(journal);
        test.cleanup(move |test| {
            note(
                &journal_for_cleanup,
                format!("cleanup {} failed={}", test.name(), test.failed()),
            );
        });
        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "a TestCase body returns a TestResult"
    )]
    fn fails_in_a_subtest(test: &super::Test, journal: &Journal) -> TestResult {
        let first = Arc::clone(journal);
        test.cleanup(move |test| {
            note(&first, format!("first cleanup failed={}", test.failed()));
        });
        let second = Arc::clone(journal);
        test.cleanup(move |test| {
            note(&second, format!("second cleanup failed={}", test.failed()));
        });
        let ok = test.run("passing case", |sub| {
            note(journal, sub.name());
            Ok(())
        });
        let bad = test.run("failing", |sub| {
            note(journal, sub.name());
            Err::<(), _>("boom").or_fatal(sub)?;
            note(journal, "after the fatal");
            Ok(())
        });
        note(journal, format!("subtests ok={ok} bad={bad}"));
        Ok(())
    }

    fn panics(_: &super::Test, _: &Journal) -> TestResult {
        panic!("a panic in the body");
    }

    fn fatal(test: &super::Test, _: &Journal) -> TestResult {
        Err(test.fatal("stopped"))
    }

    /// The suite's temporary-directory prefix.
    const PREFIX: &str = "scratch-suite-";

    /// Creates a temporary directory in a subtest, records its name, and checks it is gone once
    /// the subtest ends.
    fn makes_a_temp_dir(test: &super::Test, journal: &Journal) -> TestResult {
        let mut made = None;
        test.run("sub", |sub| {
            let dir = sub.temp_dir()?;
            assert!(dir.is_dir(), "{}", dir.display());
            made = Some(dir);
            Ok(())
        });
        let made = made.ok_or_else(|| test.fatal("the subtest made no directory"))?;
        assert!(!made.exists(), "{} outlived its subtest", made.display());
        let name = made.file_name().unwrap().to_string_lossy().into_owned();
        note(journal, name);
        Ok(())
    }

    const TESTS: &[TestCase<Journal>] = &[
        TestCase {
            name: "TestPasses",
            run: passes,
        },
        TestCase {
            name: "TestSubtests",
            run: fails_in_a_subtest,
        },
        TestCase {
            name: "TestPanics",
            run: panics,
        },
        TestCase {
            name: "TestFatal",
            run: fatal,
        },
        TestCase {
            name: "TestTempDir",
            run: makes_a_temp_dir,
        },
    ];

    /// A test's temporary directory is named with the suite's prefix, in its subtests too, and
    /// removed when the test ends.
    #[test]
    fn temp_dirs_carry_the_suite_s_prefix() {
        let (status, journal, out) = suite(&["--run", "^TestTempDir$"]);
        assert_eq!(status, 0, "{out}");
        assert_eq!(journal.len(), 2, "{journal:?}");
        assert!(journal[1].starts_with(PREFIX), "{journal:?}");
    }

    /// Runs the suite with `args`, returning the status, the journal and the output.
    fn suite(args: &[&str]) -> (u8, Vec<String>, String) {
        let journal: Journal = Arc::default();
        let buffer = Buffer::default();
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let setup_journal = Arc::clone(&journal);
        let status = run(
            PREFIX,
            &args,
            TESTS,
            move || {
                note(&setup_journal, "setup");
                Ok(setup_journal)
            },
            buffer.clone(),
        );
        let entries = journal.lock().unwrap().clone();
        (status, entries, buffer.text())
    }

    /// A selected-setup suite is told which tests are about to run, in order, before any runs.
    #[test]
    fn the_setup_is_told_the_selected_tests() {
        let journal: Journal = Arc::default();
        let setup_journal = Arc::clone(&journal);
        let args = ["--run".to_owned(), "^Test(Passes|Fatal)$".to_owned()];
        let status = super::run_selected(
            PREFIX,
            &args,
            TESTS,
            move |selected| {
                note(&setup_journal, format!("setup {}", selected.join(",")));
                Ok(setup_journal)
            },
            Buffer::default(),
        );
        assert_eq!(status, 1);
        let entries = journal.lock().unwrap().clone();
        assert_eq!(entries[0], "setup TestPasses,TestFatal");
    }

    /// A passing test runs after the setup, and its cleanup is told it passed.
    #[test]
    fn a_passing_test_passes() {
        let (status, journal, out) = suite(&["--run", "^TestPasses$", "--nocapture"]);
        assert_eq!(status, 0, "{out}");
        assert_eq!(
            journal,
            ["setup", "TestPasses", "cleanup TestPasses failed=false"]
        );
        assert!(out.contains("--- PASS: TestPasses ("), "{out}");
        assert!(out.ends_with("1 passed, 0 failed\nPASS\n"), "{out}");
    }

    /// A failed subtest fails its parent; a fatal stops only its own body; cleanups run last
    /// registered first and see the failure.
    #[test]
    fn a_failed_subtest_fails_its_parent() {
        let (status, journal, out) = suite(&["--run=TestSubtests"]);
        assert_eq!(status, 1, "{out}");
        assert_eq!(
            journal,
            [
                "setup",
                "TestSubtests/passing_case",
                "TestSubtests/failing",
                "subtests ok=true bad=false",
                "second cleanup failed=true",
                "first cleanup failed=true",
            ]
        );
        assert!(
            out.contains("    --- FAIL: TestSubtests/failing ("),
            "{out}"
        );
        assert!(out.contains("        boom\n"), "{out}");
        assert!(out.contains("--- FAIL: TestSubtests ("), "{out}");
    }

    /// The filter is matched against the subtests too.
    #[test]
    fn the_filter_selects_subtests() {
        let (status, journal, out) = suite(&["--run", "TestSubtests/passing"]);
        assert_eq!(status, 0, "{out}");
        assert!(journal.contains(&"TestSubtests/passing_case".to_owned()));
        assert!(!journal.contains(&"TestSubtests/failing".to_owned()));
    }

    /// A panic, in the body or a fatal, fails the test and the run, and the other tests still
    /// run.
    #[test]
    fn panics_and_fatals_fail() {
        let (status, _, out) = suite(&["--run", "TestPanics|TestFatal|TestPasses"]);
        assert_eq!(status, 1, "{out}");
        assert!(out.contains("panic: a panic in the body"), "{out}");
        assert!(out.contains("--- FAIL: TestFatal ("), "{out}");
        assert!(out.contains("--- PASS: TestPasses ("), "{out}");
        assert!(
            out.contains("1 passed, 2 failed: TestPanics, TestFatal"),
            "{out}"
        );
    }

    /// A `--run` pattern that selects nothing fails before the setup; a libtest filter that
    /// selects nothing passes without running it.
    #[test]
    fn selecting_nothing() {
        let (status, journal, out) = suite(&["--run", "^TestTypo$"]);
        assert_eq!(status, 1, "{out}");
        assert!(journal.is_empty(), "the setup ran: {journal:?}");
        assert!(
            out.contains(r#"--run "^TestTypo$" selects no test"#),
            "{out}"
        );
        let (status, journal, out) = suite(&["unrelated_unit_test"]);
        assert_eq!(status, 0, "{out}");
        assert!(journal.is_empty(), "the setup ran: {journal:?}");
        let (status, journal, _) = suite(&["--ignored"]);
        assert_eq!((status, journal.len()), (0, 0));
    }

    /// A failed setup fails the run before any test.
    #[test]
    fn a_failed_setup_runs_no_test() {
        let journal: Journal = Arc::default();
        let buffer = Buffer::default();
        let status = run(
            PREFIX,
            &[],
            TESTS,
            || Err::<Journal, _>("the identity check failed".to_owned()),
            buffer.clone(),
        );
        assert_eq!(status, 1);
        assert!(journal.lock().unwrap().is_empty());
        let out = buffer.text();
        assert!(
            out.contains("setup failed: the identity check failed") && !out.contains("=== RUN"),
            "{out}"
        );
    }

    /// `--list` prints the selected tests as libtest does, and runs nothing.
    #[test]
    fn list_prints_the_selected_tests() {
        let (status, journal, out) =
            suite(&["--list", "--format", "terse", "Test", "--skip", "Panics"]);
        assert_eq!(status, 0);
        assert!(journal.is_empty());
        assert_eq!(
            out,
            "TestPasses: test\nTestSubtests: test\nTestFatal: test\nTestTempDir: test\n"
        );
        let (_, _, out) = suite(&["--list", "--exact", "TestFatal"]);
        assert_eq!(out, "TestFatal: test\n");
    }

    /// libtest's flags are accepted; anything unknown is a usage error.
    #[test]
    fn arguments() {
        assert_eq!(
            parse_args(
                &[
                    "--nocapture",
                    "--test-threads=1",
                    "--color",
                    "never",
                    "-q",
                    "--run",
                    "^TestA$",
                    "-Z",
                    "unstable-options",
                ]
                .map(str::to_owned)
            ),
            Ok(Args {
                run: "^TestA$".to_owned(),
                ..Args::default()
            })
        );
        assert!(parse_args(&["--bogus".to_owned()]).is_err());
        assert!(parse_args(&["--run".to_owned()]).is_err());
        let (status, _, out) = suite(&["--bogus"]);
        assert_eq!(status, 2, "{out}");
        assert!(out.contains(r#"unknown argument "--bogus""#), "{out}");
        assert_eq!(Fatal, Fatal);
    }
}
