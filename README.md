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
it. All six are available.

| Crate | What it is | Features |
| --- | --- | --- |
| `clock` (available) | The injected wall clock: `Clock`, the system's, one that follows tokio's (paused) clock, and a `FakeClock` | – |
| `docker-client` (available) | Small shared plumbing over bollard: the connection from `DOCKER_HOST`, a whole-call timeout, an error with a kind, a stream pump, attach and wait, a fake engine | `tcp`, `tls`, `attach`, `test-util` (the `User-Agent`, the API version and the response-header timeout are parameters) |
| `testenv-core` (available) | Test-environment helpers: Go's `-run` filter, environment IDs, the sweep rules, the tool gate, git isolation, the identity rule, Go's `testing` for async and sync runners | `async`, `sync`, `bollard`, `redact` (the labels, variables, tools and runner output are parameters) |
| `repo-meta` (available) | Repository meta-test checks returning problems: workspace lints and layout, Dockerfiles and images, toolchain pins, CI gates, vacuous flags, | – (each check's strictness is a parameter) |
| `tracing-setup` (available) | A `tracing` subscriber set up the way the projects log: tracing-subscriber's text or flat JSON, no ANSI, to the caller's writer | `log-bridge`, `testing` (the writer, level, format, target and JSON span fields are parameters) |
| `service-kit` (available) | Process plumbing: `buildinfo!` stamps, the runtime and its bounded shutdown, the shutdown and hangup signals, a minimal HTTP server, the `process_*` metrics collector | `runtime`, `http`, `process-metrics` |

Where the projects legitimately differ, the shared code takes a parameter; one project's behaviour
is never forced on another.

### clock

A wall clock behind a trait, so a program reads the time only through a `Clock` it was
handed:

| Item | What it is |
| --- | --- |
| `Clock` | `now() -> jiff::Timestamp`; any `Fn() -> Timestamp + Send + Sync` closure is one |
| `SystemClock` | the system's wall clock, the one real read |
| `TokioClock` | a base time plus the `tokio::time` elapsed since it was made, so it moves with a paused runtime's clock; saturates at `Timestamp::MAX` |
| `FakeClock` | set and advanced by hand; clones share the time |

It depends on `jiff` and on `tokio` with only the `time` feature.

### docker-client

The plumbing around [bollard](https://crates.io/crates/bollard) that programs share, and none of
its calls: those stay the program's, made on the `Docker` that `connect` returns.

| Item | What it is |
| --- | --- |
| `Config`, `Config::unix` | the engine's socket (a leading `unix://` is dropped, as bollard does); `user_agent` (through bollard's request modifier, which has no setter; a value no header may carry is refused by `connect`), `response_header_timeout` (whole seconds, at least one; bollard's is 120) and `api_version` (a fixed `<major>.<minor>`, which every request then carries as `/v<major>.<minor>/...`) are the caller's |
| `Config::from_env` | `DOCKER_HOST` from the caller's lookup (`unix://`; with `tcp` also `tcp://` and `http://`; with `tls` also `https://`), `/var/run/docker.sock` when unset, and `DOCKER_API_VERSION`, which bollard does not read; any non-empty `DOCKER_TLS_VERIFY` (`0` too, as moby) turns a `tcp://` host into TLS, and an `http://` host with it set is refused as contradictory (`ConnectError::Invalid`); TLS reads `DOCKER_CERT_PATH` or `$HOME/.docker`; `DOCKER_TLS` is refused as unsupported; an empty variable is unset, a unix host ignores the TLS ones |
| `connect`, `negotiate`, `ConnectError` | the client from a `Config`; a fixed version is kept, else `negotiate_version` lowers it to the engine's. bollard drops its version prefix from every request path (`Uri::parse` joins an absolute path onto it), so `connect` installs a request modifier that puts `/v<major>.<minor>` back. One rule decides the prefix for bollard's calls and for `wait` and `events_established`: the version the `Config` fixes; else the one `negotiate` settled on, always sent, bollard's default included; else none before any negotiation (the engine's newest API, which also keeps `GET /version` answerable by an engine older than bollard's default). `negotiate(docker, &config)` returns `(Docker, Config)`: the returned `Config` records the negotiated version and is the one to give the streaming calls, so the version belongs to that client and its config, not to other clients or clones of the original `Config`; unless the version is fixed, `negotiate` asks `GET /version` on a fresh unversioned client built from the config (the `docker` given is dropped), so renegotiating with an earlier pair starts over instead of asking through a version an upgraded engine may refuse |
| `bounded` | a timeout around a whole call, body included; bollard's own covers only the response headers |
| `Error`, `ErrorKind` | `NotFound` (404), `Conflict` (409), `Timeout` (`bounded`'s, or bollard's header timeout), `Cancelled` (a token fired during the establishment of a streaming call), `Unavailable` (I/O, hyper and missing-socket errors) and `Other`; bollard's error stays reachable (`engine()`) and its cause is the `source` |
| `pump_stream` | a bollard stream (events, logs) as a channel, fed by a task that ends when the receiver is dropped, the `CancellationToken` fires or the stream ends |
| `events_established(&config, options, timeout, &cancel)`, `EventStream` | `GET /events` returned only once the engine answered 2xx, so a subscription that is stalled or refused is an error (`Timeout`, `NotFound`, ...) within the caller's timeout instead of a stream that looks live, and a fired `CancellationToken` interrupts the establishment (`ErrorKind::Cancelled`, not `Timeout`); items are `EventMessage`s, and a line that does not decode is an `Err` item that does not end the stream |
| `attach::attach`, `attach::wait` (`attach`) | a container's stdout and stderr as `Output` pieces through the pump, and its exit code under a `WaitCondition` (`NotRunning`, `NextExit`, `Removed`); `wait(&config, id, condition, timeout, cancel)` is async and returns only after the engine answered the response headers (or fails with `Timeout`, or `Cancelled` when `cancel` fires first) (it holds the waiter then, so a `start` cannot overtake it), then yields the code through a `oneshot` (a non-zero exit, which bollard reports as an error, is the code) |
| `test_util` (`test-util`) | a fake engine on a unix socket: scripted `Reply`s per path (JSON, 404, 409, an event stream that closes, an attach upgrade, a body that stalls after the headers, a delayed or silent answer, headers now and body later), and the requests it recorded; its connection tasks end with it |

`wait` and `events_established` send their request themselves, over hyper on the `Config`'s endpoint
(unix; `tcp` adds plain TCP with keepalive on, 15 seconds idle and apart and nine probes on Linux;
`tls` adds rustls presenting `cert.pem` and `key.pem` and trusting the system roots plus `ca.pem`,
exactly as bollard's ssl path does, so both kinds of call accept the same engines), because bollard
hides when a streaming call's headers arrive. They send the config's `User-Agent`.

It depends on `bollard` (pinned `=0.21.1`, only `pipe`; `tcp` adds `http`, `tls` adds `ssl`, which
is rustls on ring), `http`, `hyper` (`client`, `http1`), `hyper-util`, `http-body-util`, `bytes`,
`serde_json`, `serde_urlencoded`, `futures-util`, `thiserror`, `tokio` (`rt`, `time`, `sync`,
`macros`, `net`; `io-util` with `test-util`) and `tokio-util`; `tcp` adds `socket2`, and `tls`
adds `rustls`, `tokio-rustls`, `rustls-native-certs` and `rustls-pki-types`, all of which bollard's
`ssl` already brings.

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
| `redact` (`redact`) | no logging crate: `Log` (the trait the sweep logs through), `Logger` (plain `&str` fields to a writer, `secret` fields redacted), `Level`, `Secret`, `redact_env` |
| `host` (`bollard`) | `HostSweep {labels, resources, op_timeout}`: `sweep`, `list_environments`, `cleanup`, the removals; `sweep_rules`, `process_alive`, `with_timeout`, `OpError` |

Each consumer passes its own: swarm-scheduler-exporter and swarm-device-access build `async`,
`bollard` and `redact` with their envid bytes, labels, stale-after and suite-deadline variables,
resources and tally; bitgauge builds `sync` and the default modules; awtrix-controller,
swarm-rootless-docker and `repo-meta` the default features, for the tool gate and git isolation.
`bollard` builds bollard 0.21.1 with `pipe` only; a suite that also needs `http` or `ssl` enables
them on its own bollard dependency.

It depends on `jiff` and `regex`; `async` adds `tokio` (`rt`,
`time`, `signal`, `macros`) and `tokio-util`, `sync` adds `tempfile`, `redact` adds nothing,
and `bollard` adds `bollard` and `rustix` (`process`).

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
| `dockerfile` | the parser (`parse`, `Stage`, `Instruction`, `syntax`; an `escape` directive other than `\` or a heredoc is `Malformed`), `tag_pinned`, `digest_pinned`, `from_dhi`, `base_pinned`, `repository`, `tag`, `labels` |
| `image` | `dockerfiles` (tracked or new, `Dockerfile.<name>` included), `syntax_problems` (required or optional), `stage_problems` with a `BasePolicy` (`DhiTagElseDigest`, `DhiOnly`, `TagOnly`) |
| `toolchain` | `one_value` (one pin read from several files, over `PinSource`s and `PinValue`s), the `PinSource` presets for the Rust release (`rust_channel`, `rust_channel_line`, `rust_version`, `rust_version_line`, `rust_image_tag`, `rust_version_arg`), `rust_release` and `toolchain_targets` (from `rust-toolchain.toml`), `make_value` (make's value of a variable, `export`, `override`, `?=` and `+=` read as make reads them) |
| `ci` | `CiGates` (the result job needs every gate job; the pre-commit job skips something, and every hook it skips exists and is run by a job the result job needs), `RustdocGate`, `always_run_problems` |
| `vacuous` | `VacuousFlags` (`core`, `with_rust`): what turns a failing gate into a pass (an `or true`, `continue-on-error`, `if: false`, make's `-` prefix, `--cap-lints`, cargo's `--skip`, `--passWithNoTests`, …) and a clippy run without `-D warnings`, with `AllowedFragment` exceptions that must each be used |
| `yaml` | workflows and the pre-commit configuration as `serde_json::Value`: `action_steps`, `jobs`, `pre_commit_hooks` |
| `scratch` | `ScratchRepo` (a git-isolated temporary repository, `repo()` a `Repo::scratch`), `ScratchRepo::run` (a script run in it) |

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
| `buildinfo!(PREFIX, Defaults)` | the `<PREFIX>_VERSION`, `_COMMIT` and `_DATE` stamps, read with `option_env!` in the calling crate, as a constant `BuildInfo`; `Defaults` gives the value of each unstamped field and whether an empty stamp is kept (`EmptyStamp::Kept`) or counts as unstamped (`EmptyStamp::Unstamped`) |
| `runtime::build_runtime` (`runtime`) | the multi-thread runtime, every driver on, with the caller's worker count or tokio's default |
| `runtime::run_until_exit`, `runtime::Exit` (`runtime`) | runs the program on that runtime, then bounds the runtime's shutdown by what is left of the `Exit`'s deadline; the exit code is the program's own type |
| `signal::shutdown_signal`, `signal::lazy_shutdown_signal` (`runtime`, Unix) | SIGINT or SIGTERM, with the handlers installed when called, or when first polled |
| `signal::hangup_channel` (`runtime`, Unix) | SIGHUP into a channel holding one pending signal |
| `http::serve`, `http::ServerConfig`, `http::Outcome` (`http`) | a minimal HTTP/1 server on hyper: serves a tower service (an `axum::Router`) on a bound `TcpListener`; `TCP_NODELAY`, a header-read timeout, header count and size bounds, a write-stall timeout (a connection whose peer leaves a write pending, with no byte leaving, for that long is closed, a body streamed after the handler returned included), a transient accept-error backoff (5 ms to 1 s), optional `with_upgrades()`; returns `Drained` or `DeadlineExceeded` (`ListenerFailed` for a listener error) |
| `http::Lifecycle`, `http::Peer` (`http`) | the `stop` and `cut` tokens and the tracker of the server's tasks, also put in every request's extensions with the `Peer` address: on `stop` each connection gets `graceful_shutdown`, `serve` waits for the tracker until the drain deadline, then cancels `cut`, which drops the remaining connections; a handler that upgrades `spawn`s the upgraded task on the lifecycle and watches `cut_token()` |
| `process::ProcessCollector` (`process-metrics`) | prometheus-client's `Collector` for the nine `process_*` families of the Prometheus Go client, read from `/proc` on Linux (nothing elsewhere), with the Go client's names, types and help texts: `process_cpu_seconds_total`, `process_open_fds`, `process_max_fds`, `process_virtual_memory_bytes`, `process_virtual_memory_max_bytes`, `process_resident_memory_bytes`, `process_start_time_seconds`, `process_network_receive_bytes_total`, `process_network_transmit_bytes_total` |

Without features it has no dependency; `runtime` adds `tokio` (`rt-multi-thread`, `signal`, `sync`,
`time`, `macros`); `http` adds `hyper` (`http1`, `server`), `hyper-util`, `tokio` (`net`),
`tokio-util`, `tower-service`, `bytes` and `libc` (constants only); `process-metrics` adds
`prometheus-client`.

## Using a crate

Depend on a crate by tag. The `version` requirement is the tag's `<major>.<minor>` (`"0.1"` for
`v0.1.Z`); Cargo checks it against the crate's own version, and it keeps cargo-deny's
`wildcards = "deny"` satisfied:

```toml
[dependencies]
clock = { git = "https://github.com/leinardi/rust-common", tag = "vX.Y.Z", version = "X.Y" }
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
clock = { path = "../rust-common/crates/clock" }
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
