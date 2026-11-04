# rust-common

The Rust code shared by the `leinardi/*` Rust projects: [swarm-scheduler-exporter],
[swarm-device-access], [bitgauge], awtrix-controller and swarm-rootless-docker. Each was ported from
Go or written in Rust on its own, and large parts were copied between them and then drifted: a fix
that reached one copy did not reach the others. This repository is one Cargo workspace of small
library crates that the projects depend on as a git dependency, so a fix lands once.

The crates are not published on crates.io. They are consumed by git tag, and every crate in a tag
shares the workspace version.

[swarm-scheduler-exporter]: https://github.com/leinardi/swarm-scheduler-exporter
[swarm-device-access]: https://github.com/leinardi/swarm-device-access
[bitgauge]: https://github.com/leinardi/bitgauge

## Crates

Each crate lives under `crates/<name>` with its tests; a crate depends only on crates listed above
it. **Available:** `go-compat`, `go-clock`, `go-slog`, `service-kit`. The others are planned for
`v0.1.0` and land one at a time.

| Crate | What it is | Features |
| --- | --- | --- |
| `go-compat` (available) | Go standard-library behaviour the projects' output depends on, ported byte for byte: number and duration formatting, `strconv` quoting, the errno table, `path.Clean`, `net` address parsing, RFC 3339 time | – |
| `go-clock` (available) | The injected wall clock: `Clock`, the system's, one that follows tokio's (paused) clock, and a `FakeClock` | – |
| `go-slog` (available) | A `log/slog`-compatible logger: slog's text and JSON handlers and a plain format, byte for byte | – (the plain style, the level parsing and the global logger or a `LogHandle` are parameters) |
| `go-prom` | A Prometheus registry and the Go-compatible text exposition, with promhttp content negotiation | expfmt version as a parameter; `testing` |
| `http-framing` | HTTP/1 request-head framing, std only | – |
| `go-http` | Go `net/http` server behaviours on hyper: the timeouts, `bind` and `parse_port`, `serve` | the listen-error prefix as a parameter |
| `docker-engine` | A Docker Engine API client over HTTP/1: transport, version negotiation, events, decoding | `tcp`, `tls`, `write`, `attach`, `test-util` |
| `testenv-core` | Test-environment helpers: run filters, environment IDs, sweep rules, the tool gate, scratch git repositories | `async`, `sync`, `bollard`, `redact` |
| `repo-meta` | Repository meta-test checks: workspace lints, Markdown links, licence headers, suppressions, Dockerfiles, toolchain pins, CI gates | – |
| `tracing-setup` | A `tracing` subscriber set up the way the projects log | `log-bridge` |
| `service-kit` (available) | Process plumbing: `buildinfo!` stamps, the runtime and its bounded shutdown, the shutdown and hangup signals | `runtime` |

Where the projects legitimately differ, the shared code takes a parameter; one project's behaviour
is never forced on another.

### go-compat

Ports of go1.26's standard library, each checked against output a Go program printed (the corpora
under `crates/go-compat/tests/testdata`). One module per Go package area:

| Module | Go behaviour |
| --- | --- |
| `float` | `strconv.FormatFloat(f, 'g', -1, 64)`, which is `%v` (`format_g`, `push_g`), and encoding/json's float layout (`format_json`); exact ties round to even, as Go's do |
| `duration` | `time.Duration.String` (`format`, `push`) and `time.ParseDuration` (`parse`), on `i64` nanoseconds |
| `quote` | `strconv.Quote` (`quote`, `push_quoted`), `strconv.QuoteRune`, and Go's Unicode 15 `IsPrint` and `IsSpace` tables (`is_print`, `is_space`) |
| `time_layout` | `time.RFC3339Nano` and slog's millisecond layout, `time.Parse(time.RFC3339Nano, …)`, Go's zero time |
| `local_zone` | how Go resolves `time.Local` from `TZ` and the zoneinfo files |
| `strings` | `strings.EqualFold`, `ToLower`, `TrimSpace`, `Fields`, on Go's Unicode 15 tables |
| `parse_bool`, `parse_int`, `parse_float` | `strconv.ParseBool`, `ParseInt`, `ParseUint`, `Atoi`, `ParseFloat(s, 32)`, with Go's `*NumError` texts |
| `errno` | `syscall.Errno.Error()` from Go's whole linux/amd64 table (`errno_string`), and an `io::Error` as Go words it (`io_error_text`) |
| `errors` | `errors.Join`'s text |
| `filepath` | `filepath.Match`, `Clean`, `Join` and `Base`; on Unix `Clean` and `Join` are also `path.Clean` and `path.Join` |
| `scanner` | `bufio.Scanner` with `ScanLines` and the default 64 KiB token limit |
| `sort` | `sort.Sort` step for step, for comparisons that are not a total order |
| `http_status` | `net/http.StatusText` |
| `net_ip`, `host_port` | `net.ParseIP`, `ParseCIDR`, `IP.Equal`, `IPNet.Contains`; `net.SplitHostPort` and `JoinHostPort` |

Its only dependency is `jiff`, with `default-features = false` and `std`.

### go-clock

Go's `time.Now` behind a trait, so a program reads the wall clock only through a `Clock` it was
handed:

| Item | What it is |
| --- | --- |
| `Clock` | `now() -> jiff::Timestamp`; any `Fn() -> Timestamp + Send + Sync` closure is one |
| `SystemClock` | the system's wall clock, the one real read |
| `TokioClock` | a base time plus the `tokio::time` elapsed since it was made, so it moves with a paused runtime's clock; saturates at `Timestamp::MAX` |
| `FakeClock` | set and advanced by hand; clones share the time |

It depends on `jiff` (as `go-compat`) and on `tokio` with only the `time` feature.

### go-slog

A Go `log/slog` logger, byte for byte: slog's `TextHandler` and `JSONHandler` (go1.26), and the
plain format (`level=INFO message k=v`) of the Go programs the consumers replace, in either of its
two Go versions. It is checked against Go captures of both (`crates/go-slog/tests/testdata`).

| Item | What it is |
| --- | --- |
| `Logger`, `Config`, `Format`, `Level` | a logger with a minimum level, a format, whether the time is logged, a sink, a clock and a zone; `with_attrs` and `with_group` return a copy; one record is one write |
| `Attr`, `Value`, `Time` | typed attributes: strings, integers, floats, bools, durations, times, `[]string` (nil or not), stringers, errors, nil, groups |
| `PlainStyle::Quoted` | the plain format with one record per line: the message escaped, an unsafe key or value quoted with `strconv.Quote` |
| `PlainStyle::Raw` | the plain format as `fmt.Fprint` writes it: message and keys raw, a string quoted only for a space or a tab, keyless attributes dropped |
| `parse_level`, `parse_format`, `Config::parse` | Go's reading of a level or format name, any other value falling back to info or text |
| `parse_level_strict`, `parse_format_strict`, `ParseError` | the same names, any other value refused with `invalid log level "…" (want one of …)` |
| `LogHandle` | a cloned handle on one shared logger, swapped by `configure`, `configure_to` or `set` for every clone; `with_attrs` handles |
| `global`, `set_global`, `configure` | a process-wide logger instead |
| `MemorySink` | a sink that keeps what is written, for tests |

Every constructor takes the clock (`go_clock::Clock`); `LogHandle::stdout` and `configure` take
`getenv` to resolve the local zone. The sink is the caller's, except where Go writes to stdout:
`LogHandle::stdout`, `LogHandle::configure`, `configure` and the default `global()` write to the
process's stdout (`Logger::new`, `LogHandle::configure_to` and `set_global` take a sink). Only
`global()` before any `configure` or `set_global` reads the system clock and `TZ` itself, as Go's
default logger does.

It depends on `go-compat`, `go-clock` and `jiff` (as `go-compat`).

### service-kit

What a service's `main` repeats. Every message, exit code and default stays the caller's.

| Item | What it is |
| --- | --- |
| `buildinfo!(PREFIX, Defaults)` | the `<PREFIX>_VERSION`, `_COMMIT` and `_DATE` stamps, read with `option_env!` in the calling crate, as a constant `BuildInfo`; `Defaults` gives the value of each unstamped field and whether an empty stamp is kept (`EmptyStamp::Kept`) or counts as unstamped (`EmptyStamp::Unstamped`) |
| `runtime::build_runtime` (`runtime`) | the multi-thread runtime, every driver on, with the caller's worker count or tokio's default |
| `runtime::run_until_exit`, `runtime::Exit` (`runtime`) | runs the program on that runtime, then bounds the runtime's shutdown by what is left of the `Exit`'s deadline; the exit code is the program's own type |
| `signal::shutdown_signal`, `signal::lazy_shutdown_signal` (`runtime`, Unix) | SIGINT or SIGTERM, with the handlers installed when called, or when first polled |
| `signal::hangup_channel` (`runtime`, Unix) | SIGHUP into a channel holding one pending signal |

Without features it has no dependency; `runtime` adds `tokio` (`rt-multi-thread`, `signal`, `sync`,
`time`, `macros`).

## Using a crate

Depend on a crate by tag. The `version` requirement is the tag's `<major>.<minor>` (`"0.1"` for
`v0.1.Z`); Cargo checks it against the crate's own version, and it keeps cargo-deny's
`wildcards = "deny"` satisfied:

```toml
[dependencies]
go-compat = { git = "https://github.com/leinardi/rust-common", tag = "vX.Y.Z", version = "X.Y" }
```

A consumer's `deny.toml` allows the git source:

```toml
[sources]
allow-git = ["https://github.com/leinardi/rust-common"]
```

`Cargo.lock` records the commit the tag pointed at. Releases are `vX.Y.Z` tags, never moved once
published (see [SECURITY.md](SECURITY.md)).

The workspace's `rust-version` is the language line (`1.98`), so a consumer on any 1.98 patch
release builds it; this repository itself pins its toolchain in `rust-toolchain.toml`.

## Working on a crate and a consumer together

To try an unreleased change from a consumer, point the consumer's dependency at a local clone with a
`[patch]` section in the consumer's root `Cargo.toml`, keyed by this repository's URL:

```toml
[patch."https://github.com/leinardi/rust-common"]
go-compat = { path = "../rust-common/crates/go-compat" }
```

Every rust-common crate the consumer uses, directly or through another rust-common crate, needs its
own line, or Cargo resolves two copies. Remove the section before committing: CI cannot see the
path.

## Development

`make verify` builds, formats, lints, tests and documents the workspace, and `make check` runs every
pre-commit hook. [CONTRIBUTING.md](CONTRIBUTING.md) lists the prerequisites and the commit
conventions; [AGENTS.md](AGENTS.md) has the layout, the rules the crates keep and the validation
matrix.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Most of the code here is extracted from the author's own projects, which are licensed MIT
(swarm-scheduler-exporter, awtrix-controller, swarm-rootless-docker), Apache-2.0
(swarm-device-access) and GPL-3.0 (bitgauge). As the sole author of that code, Roberto Leinardi
relicenses it here under MIT OR Apache-2.0. The projects themselves keep their own licenses.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this repository by you, as defined in the Apache-2.0 license, shall be dual-licensed as above,
without any additional terms or conditions.
