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
it. All eleven are available.

| Crate | What it is | Features |
| --- | --- | --- |
| `go-compat` (available) | Go standard-library behaviour the projects' output depends on, ported byte for byte: number and duration formatting, `strconv` quoting, the errno table, `path.Clean`, `net` address parsing, RFC 3339 time | – |
| `go-clock` (available) | The injected wall clock: `Clock`, the system's, one that follows tokio's (paused) clock, and a `FakeClock` | – |
| `go-slog` (available) | A `log/slog`-compatible logger: slog's text and JSON handlers and a plain format, byte for byte | – (the plain style, the level parsing and the global logger or a `LogHandle` are parameters) |
| `go-prom` (available) | A Prometheus registry and the Go-compatible text exposition, with promhttp content negotiation and the process collector | `testing` (the expfmt version is a parameter) |
| `http-framing` (available) | HTTP/1 request framing over the bytes a connection read, as hyper frames them, std only | – |
| `go-http` (available) | Go `net/http` server behaviours on hyper: the mux's answers, header order and framing, the four timeouts, `bind` and `parse_port`, `serve` and its drain | `testing` (the timeouts, the routes, the log callback and the listen-error prefix are parameters) |
| `docker-engine` (available) | A Docker Engine API client over HTTP/1 that sends what moby client v0.6.0 sends: transport, version negotiation, events, decoding, moby's error texts | `tcp`, `tls`, `write`, `attach`, `test-util` (the `User-Agent` and the response-header timeout are parameters) |
| `testenv-core` (available) | Test-environment helpers: Go's `-run` filter, environment IDs, the sweep rules, the tool gate, git isolation, the identity rule, Go's `testing` for async and sync runners | `async`, `sync`, `bollard`, `redact` (the labels, variables, tools and runner output are parameters) |
| `repo-meta` (available) | Repository meta-test checks returning problems: workspace lints and layout, Markdown links, Dockerfiles and images, toolchain pins, CI gates, lint allowlists, vacuous flags, and the licence-header and suppression script harnesses | – (each check's strictness is a parameter) |
| `tracing-setup` (available) | A `tracing` subscriber set up the way the projects log: tracing-subscriber's text or flat JSON, no ANSI, to the caller's writer | `log-bridge`, `testing` (the writer, level, format, target and JSON span fields are parameters) |
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

### docker-engine

A Docker Engine API client on hyper's HTTP/1 client, as moby client v0.6.0 talks to the daemon:
the same requests, headers and API version negotiation, and the same error texts. The calls a
program makes and the types it decodes stay the program's; it builds them on the generic calls.

| Item | What it is |
| --- | --- |
| `Settings`, `Settings::unix` | the daemon's socket (moby's `WithHost("unix://…")`); `user_agent` (moby's own, `moby_user_agent()`, by default; a value no header may carry is refused, `check_user_agent` tests one first) and `response_header_timeout` (none by default; it also bounds the body reads of the negotiation's ping) are the caller's |
| `Settings::from_env` (`tcp`) | `DOCKER_HOST` (unix, TCP and its base path, scoped IPv6), `DOCKER_API_VERSION` (a pin, no negotiation) and, with `tls`, `DOCKER_CERT_PATH` and `DOCKER_TLS_VERIFY`, an empty one refused |
| `Client` | the pool (two idle connections for 30s, a `GET` or `HEAD` sent again on a dropped reused connection, any other request only if it was never written), the lazy negotiation down from 1.56 with one shared ping, gzip on TCP, moby's error wording with the cause kept as the `source` |
| `Client::get`, `get_body`, `get_list`, `get_object`, `events`, `ping` | a versioned `GET`; Go's decoding of a list or an object; moby's event stream; moby's `Ping` |
| `Client::exchange` (`write`) | a versioned request of any method and body from the one builder |
| `Client::exchange_upgrade`, `stdcopy`, `wait` (`attach`) | moby's hijacked attach on a connection of its own, its `StdCopy`, and the wait answer's parser |
| `Error`, `ErrorKind`, `ConfigError`, `PathError` (`tls`) | moby's texts; the `io::Error`, `hyper::Error` or `serde_json::Error` behind one is its `source` |
| `context`: `CallContext`, `with_context`, `with_context_timeout`, `current` | Go's `context.Context` around a call; a request it cuts short fails as net/http's `url.Error`; `current` lets a fake engine read the caller's deadline and cancellation (task-local) |
| `with_bare_timeout`, `with_bare_deadline` | a bound around a call the client does not see: the bare `context deadline exceeded` (swarm-scheduler-exporter's `with_timeout`/`with_deadline`; swarm-device-access's `with_timeout` is `with_context_timeout`) |
| `events::JsonStream` | moby's JSON stream decoder (the RS filter, `io.EOF` against `io.ErrUnexpectedEOF`), each record bounded to 1 MiB |
| `types` | `decode_first`, `decode_list`, `null_default`, `Nullable`, `Labels`: Go's `encoding/json` on the answers |
| `version` | `negotiate`, `parse_api_version`, `compare`, `validate_client_api_version`, `MIN_API_VERSION` 1.40, `MAX_API_VERSION` 1.56 |
| `Filters`, `encode_query`, `since_parameter`, `trim_id`, `push_json_string` | moby's request parameters |
| `test_util` (`test-util`) | a minimal HTTP/1.1 fake engine on TCP or a unix socket with the test's own responder, the request recorder's normalisation, `OrPanic`, `TempDir`, `eventually` |

Each consumer passes its own behaviour: swarm-scheduler-exporter builds `tcp` and `tls`, names
itself in the `User-Agent` and bounds the response headers to 30s; swarm-device-access builds
`write` and `attach` and keeps moby's `User-Agent` and no header timeout.

It depends on `go-compat`, `jiff` (as `go-compat`), `hyper` (`client`, `http1`), `hyper-util`
(`tokio`), `http-body-util`, `bytes`, `tokio` (`net`, `time`, `rt`, `sync`, `macros`; `io-util`
with `attach` and `test-util`), `tokio-util`, `serde` and `serde_json`; `tcp` adds `socket2` and
`flate2` (the `rust_backend` only), and `tls` adds `rustls` on `ring`, `tokio-rustls`,
`rustls-pki-types` and `rustls-native-certs`.

### testenv-core

What the projects' test environments share: their Docker suites (`harness = false` runners with
Go's test output) and their meta-tests. With no feature it needs no Docker, network or runtime.
The suites' tests, clusters, images, label names and variables stay theirs; they pass them in.

| Item | What it is |
| --- | --- |
| `run_filter::RunFilter` | Go's `-run` (levels split on `/`, alternatives on a top-level bar, unanchored); a pattern that selects nothing is an error |
| `Error` | a message with Go's `%w` context chain and `errors.Join` |
| `envid::EnvIds` | `{bytes, label, prefix}`: `generate`, `resource_name`, `label_filter` (Engine API), `cli_filter` (`label=…`); `random_hex` |
| `sweep_rules` | `SweepRules::decide` (stale, same host and PID namespace, owner dead), `Owner`, `parse_owner`, `current_owner`, `hostname`, `current_pid_namespace`, `stale_after_from_env(name, default, getenv)`, `Labels` (`run_labels`, `record`), `Resources::cleanup_command` |
| `toolgate::Gate` | `{require_env, tools, locate, note}`: a tool on `PATH` or under the nearest ancestor's directory, a skip with a note or, with the variable at `1`, a failure (`Required`); `lookup`, the testable `lookup_in`; `repo_root!()` |
| `gitenv` | `isolate(&mut Command)`: no `GIT_*` redirection, no user or system config; `REPOSITORY_VARS`, `redirects` |
| `identity` | `IDENTITY_FLAG` (`-help`), `Answer` and `Text` (an implementation's whole answer), `excerpt` (Go or Rust quoting), `is_static_elf` |
| `context` (`async`) | Go's `context.Context` on tokio: `with_timeout`, `with_cancel`, `without_cancel`, `run` |
| `testing` (`async`) | `TestHandle` (`log`, `error`, `fatal`, `skip`, `cleanup`, `run`), `Fatal`, `OrFatal`, `Outcome`, `Test`, `Suite<S>`, and `Runner` over a `RunnerConfig` (`--run`, the suite deadline, SIGINT and SIGTERM, the tally) |
| `runner` (`sync`) | bitgauge's runner: `TestCase<S>`, `Test`, `main`, `main_selected`, `run`, `run_selected`, each given the prefix of the tests' temporary directories (bitgauge's `bitgauge-test-`); libtest's flags accepted |
| `redact` (`redact`) | `RedactingLogger` (Go's text handler at debug, the caller's clock and zone), `Secret`, `redact_env` |
| `host` (`bollard`) | `HostSweep {labels, resources, op_timeout}`: `sweep`, `list_environments`, `cleanup`, the removals; `sweep_rules`, `process_alive`, `with_timeout`, `OpError` |

Each consumer passes its own: swarm-scheduler-exporter and swarm-device-access build `async`,
`bollard` and `redact` with their envid bytes, labels, stale-after and suite-deadline variables,
resources and tally; bitgauge builds `sync` and the default modules; awtrix-controller,
swarm-rootless-docker and `repo-meta` the default features, for the tool gate and git isolation.
`bollard` builds bollard 0.21.1 with `pipe` only; a suite that also needs `http` or `ssl` enables
them on its own bollard dependency.

It depends on `go-compat`, `jiff` (as `go-compat`) and `regex`; `async` adds `tokio` (`rt`,
`time`, `signal`, `macros`) and `tokio-util`, `sync` adds `tempfile`, `redact` adds `go-slog`, and
`bollard` adds `bollard` and `rustix` (`process`).

### repo-meta

The checks the projects' meta-tests repeat. Each takes the repository (`Repo::new(root)`, the root
from `testenv_core::repo_root!()`) or the text it checks and returns `Vec<Problem>`, empty when
the repository passes; a check never panics on what it reads, and input on which it would pass
having checked nothing (no file, no rule) is itself a problem. The caller asserts with
`assert!(problems.is_empty(), "{}", report(&problems))`. What differs between the projects (the
members, the pins, the jobs, the hooks, the exemptions) is a parameter; every check is as strict
as the strictest project's.

`repo_root!()` is fixed at compile time: a test binary checks the checkout it was built from. A
scratch copy or a second worktree builds into a target directory of its own (or cleans the
shared one first), or its binary goes on checking the copy it was built from.

What stays in a consumer's own meta-tests: swarm-rootless-docker's exact dind image, last-USER,
single user-rewrite and no-apk rules and its `base.name` label; awtrix-controller's and
swarm-rootless-docker's `label_arguments_are_supplied`; and every check that runs a tool, such as
droast.

| Module | What it checks |
| --- | --- |
| `Repo`, `Problem`, `report` | the repository under test (`Repo::new`, git with the process's environment, so a commit hook's index) or a scratch one (`Repo::scratch`, git isolated); the tracked files (`tracked`, `tracked_or_new`, `files_under`), reads that become problems (`try_read`); `file:line: message` problems |
| `workspace` | `lint_opt_in` (the root's `[workspace.lints]`, every member's `[lints] workspace = true`, the expected members), `lint_level_problems`, `override_problems`, `layout_problems` (crates under one directory, nothing else tracked there) |
| `links` | `check_repository` (every tracked Markdown file, none at all a problem) and `Checker`: every relative link resolves to a file, and every `#anchor` to a heading, as the Go link checker the projects ported reads them |
| `dockerfile` | the parser (`parse`, `Stage`, `Instruction`, `syntax`; an `escape` directive other than `\` or a heredoc is `Malformed`), `tag_pinned`, `digest_pinned`, `from_dhi`, `base_pinned`, `repository`, `tag`, `labels` |
| `image` | `dockerfiles` (tracked or new, `Dockerfile.<name>` included), `only_dockerfile_problems`, `syntax_problems` (required or optional), `stage_problems` with a `BasePolicy` (`DhiTagElseDigest`, `DhiOnly`, `TagOnly`), `BuildStage` (a `BuildBase` repository or exact image, each `Download` checked in its RUN: release, platform argument, exact arms, sha256 check; every curl or wget, of a URL or a variable, a rule's and checked in its RUN; no ADD of a URL or a variable), `Stamps` (each read by the version module, with `option_env!` or through service-kit's `buildinfo!`, whose fields are `BUILDINFO_FIELDS`; in the building RUN, the Makefile's winning prefix, `rust.mk`'s `RUST_VERSION_ENV`, `rust-dist` and `rust-run` (`rust-build` may build unstamped), the release recipe), `RuntimeStage`, `label_problems`, `platform_problems` (in the named workflow), `label_policy_problems` |
| `toolchain` | `one_value` (one pin read from several files), `RustVersion::{Exact, Line}`, `image_rust_problems` (the FROM tag equal to `ARG RUST_VERSION`), `ci_toolchain_problems` with a `ToolchainRef` (`Input`, or `InputOrRef` for dtolnay's `@<release>`), `rust_std_problems`, `make_value` (make's value of a variable, `export`, `override`, `?=` and `+=` read as make reads them), `ReleasePins` and `release_pin_problems` (exact `(platform, name, digest)` arms, no stray `sha256`), `zigbuild_action_problems`, `shellcheck_problems`, `trivy_problems` |
| `ci` | `CiGates` (the result job needs every gate job; the pre-commit job skips something, and every hook it skips exists and is run by a job the result job needs), `RustdocGate`, `always_run_problems` |
| `allowlist` | `LintAllowlist`: a lint (`unsafe_code`, a clippy ban) lifted only where an exemption names the file or item, at most once there, every exemption used unless marked `reserved`, and a group that contains the lint never named |
| `vacuous` | `VacuousFlags` (`core`, `with_rust`, `with_javascript`): what turns a failing gate into a pass (an `or true`, `continue-on-error`, `if: false`, make's `-` prefix, `--cap-lints`, cargo's `--skip`, `--passWithNoTests`, …) and a clippy run without `-D warnings`, with `AllowedFragment` exceptions that must each be used |
| `integration` | `IntegrationGate` (integration modules behind a feature, the suite's manifest entry), `cargo_test_commands`, `docker_drivers` |
| `tools` | `tool_lookup_problems`: no meta-test looks a tool up on `PATH` or reads a switch that turns a missing tool into a skip, for a project whose meta-tests fail without their tools (one on `toolgate::Gate` does not run it) |
| `yaml` | workflows and the pre-commit configuration as `serde_json::Value`: `action_steps`, `jobs`, `pre_commit_hooks` |
| `scratch` | `ScratchRepo` (a git-isolated temporary repository, `repo()` a `Repo::scratch`), `HeaderHarness` and `HeaderCase` (`scripts/check-license-headers.sh` on the repository and on scratch cases, at least one accepted and one rejected), `SuppressionHarness` and `RatchetCase` (`scripts/check-suppressions.sh` and its baseline) |

It depends on `testenv-core` (default
features: git isolation), `regex`, `serde-saphyr` and `serde_json` (the YAML), `toml` and `tempfile`.

### tracing-setup

The subscriber awtrix-controller, swarm-rootless-docker and bitgauge each built: tracing-subscriber's
fmt layer, no ANSI, RFC 3339 UTC timestamps (optional), text or one flat JSON object per line. The caller
picks the writer (stdout, stderr, a bounded lane), the level, the format and the options; the crate
parses no configuration.

| Item | What it is |
| --- | --- |
| `build(writer, Format, level, Options) -> Dispatch` | the subscriber; `Format::Text` or `Format::Json` (`flatten_event`) |
| `Options::new().target(bool).span_fields(bool).time(bool)` | the target before the message (text) or as `"target"` (JSON), the JSON `"span"`/`"spans"` fields, and the timestamp (`shows_time()`); all shown by default, tracing-subscriber's own default; bitgauge turns the first two off, a process whose supervisor stamps the lines turns `time` off |
| `build_reloadable(writer, Format, level, Options) -> (Dispatch, ReloadHandle)` | the same subscriber with a `reload` layer for the format and another for the level; the writer must be `Clone` |
| `ReloadHandle` | `reconfigure(Format, level)`, `set_level`, `set_format`, each `Result<(), ReloadError>`; `Clone + Send + Sync`, usable after the `Dispatch` is installed globally (a SIGHUP reload); it fails once the subscriber is dropped |
| `install_global(Dispatch)` | the process's global default; `InstallError` when one is installed |
| `install_log_bridge()` (`log-bridge`) | routes the `log` facade in, each record with its own target, file and line (tracing-subscriber's `tracing-log` feature) |
| `testing::Capture` (`testing`) | a writer keeping what a test logged: `bytes`, `text`, `json_lines` |
| `testing::set_default(&Dispatch)` (`testing`) | a scoped default that never loses a record to tracing's interest cache when tests run in parallel |

### service-kit

What a service's `main` repeats. Every message, exit code and default stays the caller's.

| Item | What it is |
| --- | --- |
| `buildinfo!(PREFIX, Defaults)` | the `<PREFIX>_VERSION`, `_COMMIT` and `_DATE` stamps, read with `option_env!` in the calling crate, as a constant `BuildInfo`; `Defaults` gives the value of each unstamped field and whether an empty stamp is kept (`EmptyStamp::Kept`) or counts as unstamped (`EmptyStamp::Unstamped`); repo-meta's `Stamps` counts a call as reading the three |
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
