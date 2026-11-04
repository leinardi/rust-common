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
it. **Available:** `go-compat`, `go-clock`, `go-slog`, `go-prom`, `http-framing`, `go-http`,
`service-kit`. The others are planned for `v0.1.0` and land one at a time.

| Crate | What it is | Features |
| --- | --- | --- |
| `go-compat` (available) | Go standard-library behaviour the projects' output depends on, ported byte for byte: number and duration formatting, `strconv` quoting, the errno table, `path.Clean`, `net` address parsing, RFC 3339 time | – |
| `go-clock` (available) | The injected wall clock: `Clock`, the system's, one that follows tokio's (paused) clock, and a `FakeClock` | – |
| `go-slog` (available) | A `log/slog`-compatible logger: slog's text and JSON handlers and a plain format, byte for byte | – (the plain style, the level parsing and the global logger or a `LogHandle` are parameters) |
| `go-prom` (available) | A Prometheus registry and the Go-compatible text exposition, with promhttp content negotiation and the process collector | `testing` (the expfmt version is a parameter) |
| `http-framing` (available) | HTTP/1 request framing over the bytes a connection read, as hyper frames them, std only | – |
| `go-http` (available) | Go `net/http` server behaviours on hyper: the mux's answers, header order and framing, the four timeouts, `bind` and `parse_port`, `serve` and its drain | `testing` (the timeouts, the routes, the log callback and the listen-error prefix are parameters) |
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

`Logger::new`, `LogHandle::stdout` and `configure` take the clock (`go_clock::Clock`);
`LogHandle::new` and `set_global` take a `Logger`, and so its clock, and `LogHandle::configure` and
`configure_to` keep the current logger's. `LogHandle::stdout` and `configure` take `getenv` to
resolve the local zone. The sink is the caller's, except where Go writes to stdout:
`LogHandle::stdout`, `LogHandle::configure`, `configure` and the default `global()` write to the
process's stdout (`Logger::new`, `LogHandle::configure_to` and `set_global` take a sink). Only
`global()` before any `configure` or `set_global` reads the system clock and `TZ` itself, as Go's
default logger does.

It depends on `go-compat`, `go-clock` and `jiff` (as `go-compat`).

### go-prom

The parts of the Prometheus Go client (`client_golang` v1.24.1) the Go programs used, byte for
byte: a registry, its families, the text exposition format 0.0.4 and promhttp's answer.

| Item | What it is |
| --- | --- |
| `Registry`, `Collector`, `Desc`, `Kind`, `Gathered` | a registry gathered as Go's `Gather` normalises: label pairs by name, series by label values, families by name, empty families dropped; a collector's descriptors are read once, at registration, and no collector callback runs under the registry's lock |
| `Gauge`, `GaugeVec`, `Counter`, `CounterVec`, `Histogram`, `GaugeFunc`, `DEF_BUCKETS` | the families, with Go's label checks (`LabelsMismatch`) |
| `SnapshotCollector`, `SnapshotFamily`, `SnapshotBuilder`, `snapshot::series_key` | gauge families rebuilt whole off to the side and published with one swap, so a scrape never sees half an update |
| `constant_desc` | a descriptor of constant names that panics on a typo |
| `text` | expfmt's `MetricFamilyToText` (`encode`, `push_float`, `TEXT_CONTENT_TYPE`) |
| `exposition` | promhttp's `Accept` negotiation (`negotiate`, `content_type`, `go_content_type`), its `Accept-Encoding` negotiation (`negotiate_encoding`) and the answer (`render`), gzipped with Go's header bytes |
| `ExpfmtVersion::V0_70_1`, `V0_71_0` | the `prometheus/common` release whose negotiation to follow: v0.70.1 matches exact types only, v0.71.0 lets a wildcard type or subtype match |
| `process::ProcessCollector` | the nine `process_*` families, read from `/proc` with procfs's quirks |
| `promhttp::HandlerMetrics` | `promhttp_metric_handler_requests_total` and `_in_flight`, in Go's order |
| `testing` (feature) | the Go tests' `gather_series` and `series_id`, for a program's own collector tests |

Each `ExpfmtVersion` is checked against Go's answers: 95 header pairs under v0.71.0 and 106 under
v0.70.1 (`crates/go-prom/tests/testdata/exposition`). The body is always text 0.0.4; only the
`Content-Type`'s `escaping` follows the negotiation. The `go_*` runtime families are not served.

It depends on `go-compat` and `flate2` (the `rust_backend` only).

### http-framing

`Framer` follows the bytes an HTTP/1 connection read, with hyper's framing rules (empty lines
before a request, the head, then a `Content-Length` or chunked body), and counts the requests that
have started (`started`). hyper reads ahead, so the start of the next request can already sit in
its buffer when a response ends; a server's deadlines read this count to tell that from an idle
connection, as Go's server does with `bufr.Peek`. It only counts: parsing and rejecting requests
stays hyper's.

It has no dependency beyond the standard library.

### go-http

A Go `http.ServeMux` behind an `http.Server`, on hyper's HTTP/1 server, byte for byte where hyper
can (go1.26). The routes, the handlers and what the program logs stay the program's: it has no
dependency on `go-slog` or `go-prom`.

| Item | What it is |
| --- | --- |
| `Mux`, `Reply`, `Handler` | exact one-segment routes (`route`, `route_async`), any method; `OPTIONS *`, `CONNECT`, Go's 404, the 307 to the cleaned path and the 400 as Go's mux answers them; a reply's headers in Go's order, `Reply::sniffed` for a type Go sniffs, `Reply::metrics` for promhttp's answer |
| `MetricsRequest` | what promhttp reads from a scrape (`Accept`, `Accept-Encoding`, `name[]`), for the program's own `/metrics` handler |
| `Server`, `connection` | the mux, the clock `Date` is read from and a log callback, served on one connection with Go's header case, order and chunking |
| `Timeouts`, `ConnState`, `TimedIo` | `ReadHeaderTimeout`, `ReadTimeout`, `WriteTimeout` and `IdleTimeout` kept by an I/O wrapper, with `http-framing` telling a pipelined request from an idle connection; no default, each program passes its own |
| `serve`, `Served`, `Drained` | the accept loop with Go's retry (logged through the callback, `log_printf_line` gives Go's prefix), a fatal accept error handed to `on_error`, and the drain to the caller's deadline |
| `bind`, `parse_port`, `ListenError` | `net.Listen("tcp", …)`: the dual-stack wildcard (an empty host and the unspecified literals), IPv4-mapped addresses, IPv6 zones, names, Go's built-in TCP service names and Go's error texts, after the caller's prefix |
| `gopath` | `net/url`'s escaping and `ParseQuery`, the mux's `cleanPath` (on `go_compat::filepath::clean`), `escape_host` |
| `testing` (feature) | Go wire captures (`Capture`, `read_captures`) replayed against a `Server` (`replay`, `exchange`) |

It depends on `http-framing`, `go-compat`, `go-clock`, `jiff` (as `go-compat`), `hyper` (`http1`,
`server`), `hyper-util` (`tokio`, `http1`, `server`, `server-graceful`), `bytes`, `tokio` (`net`,
`time`, `rt`, `macros`), `socket2` and `rustix` (`net`, for a zone's interface index).

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
