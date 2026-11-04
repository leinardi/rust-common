# AGENTS.md

## What this is

A Cargo workspace of small library crates shared by the `leinardi/*` Rust projects
(swarm-scheduler-exporter, swarm-device-access, bitgauge, awtrix-controller, swarm-rootless-docker),
consumed as a git dependency by `vX.Y.Z` tag and never published on crates.io. Most of the code is
extracted from those projects, where copies had drifted apart; here it lands once, with the union of
the fixes. The README is the user-facing contract: the crate list, how to depend on a tag, the
`[patch]` workflow and the licence. Edition 2024, toolchain pinned in `rust-toolchain.toml`,
`rust-version = "1.98"`, licensed MIT OR Apache-2.0.

## Common commands

Build / test / lint use Make wrappers around `cargo`, workspace-wide and `--locked`:

```bash
make verify               # rust-build, rust-fmt-check, rust-clippy, rust-test and rust-doc
make rust-build           # cargo build --workspace --all-targets --all-features
make rust-test            # cargo test --workspace, the meta-tests included
make rust-clippy          # clippy over every target and feature, warnings as errors
make rust-fmt             # cargo fmt --all (rust-fmt-check only checks)
make rust-doc             # cargo doc --workspace --all-features --no-deps, warnings as errors
make rust-feature-matrix  # each crate on its own under every set in feature-sets.toml
make rust-deny            # cargo-deny: advisories, licences, bans and sources (network required)
make audit-deps           # cargo-deny advisories only (network required)
make check                # pre-commit on all files: the local gate, as CI's pre-commit-all
make check-stage          # pre-commit on the staged files only
make pre-commit-install   # installs the pre-commit and commit-msg hooks
make help                 # lists every target
```

`rust-run` and `rust-dist` come with make-common's `rust.mk` but do not apply: there is no binary.

Single test: `cargo test -p <crate> <name>`, or `cargo test -p meta --test docs`.

The Makefile pulls shared snippets from `leinardi/make-common@v1` into `.mk/` on first run,
`rust.mk` (the `rust-*` targets and `verify-rust`) among them. To refresh:
`make mk-common-update`. Project targets live in the local `.mk/*.mk` files listed in
`MK_LOCAL_FILES` (`verify`, `audit-deps`, `rust-feature-matrix`), never as recipes in the Makefile.

## Layout

| Path | What lives there |
| --- | --- |
| `Cargo.toml` | the workspace: members listed explicitly (never a glob), `[workspace.package]`, the one `[workspace.lints]` set |
| `crates/<name>/` | the library crates, one directory each, named as the package; each is a workspace member and has a `feature-sets.toml` entry |
| `meta/` | the meta-tests, which check the repository rather than the crates (`meta/tests/`), their helpers (`meta/src`), and the `feature-matrix` runner (`meta/src/bin`) |
| `feature-sets.toml` | the per-crate feature sets `make rust-feature-matrix` lints (`-D warnings`) and tests; its format is documented at its top |
| `deny.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml` | cargo-deny policy, clippy settings (no application bans), formatting, the pinned toolchain |
| `Makefile`, `.mk/`, `scripts/bootstrap-mk-common.sh` | make-common: shared snippets in `MK_COMMON_FILES`, local ones in `MK_LOCAL_FILES` |
| `.github/workflows/ci.yaml` | `pre-commit-all`, `rust`, `feature-matrix`, `conventional-commits` and the `ci-result` aggregator; the reviewdog jobs on pull requests |
| `.github/workflows/release.yaml` | the **Release** workflow, on gh-reusable-workflows' `simple-tag-and-release` (`vX.Y.Z` tags) |
| `.github/workflows/pre-commit-warmup.yaml` | refreshes the pre-commit cache on `main` (hook config change, weekly) and prunes the older entries |
| `scripts/` | the licence-header check, the suppression ratchet, the make-common bootstrap and the Claude Code `rustfmt` hook (`scripts/hooks/rustfmt-on-edit.sh`, wired in `.claude/settings.json`) |
| `.agents/skills/` | project skills; `.claude/skills` is a symlink to it |

The meta-tests (`cargo test -p meta`) need nothing beyond cargo and git: every workspace member
opts into the workspace lints (`workspace_lints.rs`), every relative Markdown link resolves
(`docs.rs`), and `feature-sets.toml` covers every crate and every feature it declares
(`feature_sets.rs`). Each check has a negative control over a scratch tree, so it is not trusted on
the strength of having found nothing to check.

### Generated or vendored: never hand-edit

- `.mk/help.mk`, `.mk/password.mk`, `.mk/pre-commit.mk`, `.mk/rust.mk`, `.mk/.mk-common-version`
  and `scripts/bootstrap-mk-common.sh`: fetched from make-common; refresh them with
  `make mk-common-update` and port a change upstream.
- `crates/*/tests/testdata/**`: byte-for-byte captures and golden files (`.gitattributes` marks them
  `-text`; the whitespace hooks skip them).

## Rules for the crates

- **Provenance.** Each extracted file carries, in its module doc, the line
  `Extracted from <repo>/<path> at <sha>` naming the copy it came from (the best one, with the
  other copies' fixes merged in). A file assembled from several copies names each. The reasoning
  behind a merge lives in the commit body.
- **A behaviour difference between consumers is a parameter, never a fork.** Where two projects
  legitimately behave differently (an exposition format version, a log style, a timeout, a user
  agent), the crate takes a parameter or a feature and each project passes its own value. One
  project's behaviour is never forced on another, and a crate never grows a copy per consumer.
- **No application policy.** A crate does not log to the process's output, read the wall clock
  behind the caller's back or pick a consumer's defaults: it takes a writer, a clock and settings.
  The consumers' clippy bans are theirs; `clippy.toml` here bans nothing, and a ban added later
  comes with `disallowed_methods = "deny"` in `[workspace.lints.clippy]`.
- **Dependencies run downward.** A crate depends only on the crates before it in the README's
  table, and adds a dependency only for what it uses, with `default-features = false` where the
  defaults pull in more.
- **Every feature is built alone.** A new crate is added to the workspace `members` and to
  `feature-sets.toml` in the same change, with a `{ no-default-features = true }` set, an
  `{ all-features = true }` set and each exact set a consumer uses.
- **Lints.** Every member opts in with `[lints] workspace = true`. Lints are silenced only with
  `#[expect(lint, reason = "...")]`: `allow_attributes` is denied, the reason is mandatory, and an
  `#[expect]` that stops firing fails the build.
- **Versions.** Every crate takes `version.workspace = true`, so a tag carries one version for all
  of them; a consumer pins the tag and requires `version = "0.1"`.

## Licence headers and suppressions

Every Rust and shell source, the Makefile and every repo-local `.mk` snippet carries the
MIT OR Apache-2.0 header, with the year of the file's first commit
(`scripts/check-license-headers.sh`, the `check-license-headers` hook). Copy it from an existing
file of the same kind; `.idea/copyright/` holds the same text for IntelliJ. A file extracted from a
consumer gets this header in place of its original one, whatever that was (MIT, Apache-2.0 or
GPL-3.0): the author relicenses it here. The make-common files are exempt: they carry make-common's
own header.

Every in-file suppression is counted per rule against `.suppressions-baseline` by the
`check-suppressions` hook (`scripts/check-suppressions.sh`), in three families: each lint of an
`#[expect(...)]` or `#[allow(...)]`, and `# shellcheck disable=`. A commit that adds one, swaps one
or moves one must change the baseline too, and so must one whose count fell below it: run
`scripts/check-suppressions.sh --update` and commit the baseline with the change.

## Conventions

- Pin every action and reusable workflow by full commit SHA, with the version in a trailing comment
  (`@<sha> # vX.Y.Z`). The toolchain step pins the same release as `rust-toolchain.toml`.
- Give every workflow a top-level `permissions: contents: read`, and widen it only on the job that
  needs more.
- Pass `${{ }}` expressions into `run:` scripts through `env:`, never inline them in the script.
- A new gating CI job is added to `ci-result`'s `needs`. A hook that needs a system tool gets that
  tool installed in `pre-commit-all`; a hook that a dedicated job runs instead is listed in
  `pre-commit-all`'s `SKIP`, with the job named in a comment.
- Prose in Markdown wraps at 100 columns (markdownlint `line-length`, `.editorconfig`); tables, code
  blocks and headings may run longer.

## Commit messages

All commits MUST be Conventional Commits 1.0.0 **with a scope**:
`<type>(<scope>)[!]: <description>`, optional blank-line body and footers. Enforced by the
`conventional-pre-commit` `commit-msg` hook (`--force-scope`, installed by
`make pre-commit-install`) and by the `conventional-commits` CI job. Types: `feat`, `fix`, `docs`,
`test`, `refactor`, `perf`, `build`, `ci`, `chore`, `style`, `revert`. The scope is the crate
(`fix(go-prom): ...`, `feat(docker-engine): ...`) or the area (`ci(rust)`, `build(make)`,
`docs(readme)`). Breaking changes use `!` before `:` or a `BREAKING CHANGE:` footer. Release notes
are not built from these messages: `gh release create --generate-notes` lists the merged pull
requests by title.

A release with no explicit version is derived by `svu` from the commits since the last `vX.Y.Z` tag,
so a wrong type ships a wrong version:

| Release | Commit | Example |
| --- | --- | --- |
| major | any type with `!` before the colon, or a `BREAKING CHANGE:` footer | `feat(go-slog)!: take the level parser as a parameter` |
| minor | `feat` | `feat(docker-engine): add the attach feature` |
| patch | `fix` | `fix(go-http): enforce an earlier deadline at once` |
| none | everything else: `perf`, `refactor`, `build`, `ci`, `chore`, `docs`, `style`, `test`, `revert` | `docs(readme): fix a link` |

The highest bump among the commits wins; with only "none" commits since the last tag, a release
with no version fails with "nothing to bump".

**Pick the type by whether the change should ship, not by what kind of change it is.** Anything
that changes what a consumer builds when it moves to the next tag, and that it should receive, is
`fix` (or `feat`), even when it is a performance improvement, a refactor or a revert. A change to a
public signature that breaks a consumer is a `!`. Use `perf`, `refactor`, `style` and `revert` only
when the commit is deliberately not meant to trigger a release on its own. A `revert` of a shipped
`feat` or `fix` is itself a `fix`. `svu` matches `feat`/`fix` anywhere in the subject (e.g.
`prefix:` counts as `fix:`), so avoid a word ending in `feat` or `fix` directly before a colon in
other subjects. PRs land as merge commits, so every commit counts, not just the PR title. See
[`CONTRIBUTING.md`](CONTRIBUTING.md#releasing).

## Validation matrix

Run before considering an edit done. `make check` applies to every row.

| You edited | Run |
| --- | --- |
| `**/*.rs`, `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml` | `make verify` |
| a crate's `[features]`, its dependencies, `feature-sets.toml` | `make rust-feature-matrix`, `cargo test -p meta --test feature_sets` |
| `Cargo.toml`, `Cargo.lock`, `deny.toml` | `make rust-deny` (network) |
| `.github/workflows/**`, `.github/actions/**`, `.github/dependabot.yml` | `pre-commit run actionlint --all-files`; reason through each trigger the workflow has (pull request, push, dispatch, schedule) |
| `.pre-commit-config.yaml`, `Makefile`, `.mk/*.mk` in `MK_LOCAL_FILES` | `make check`, `make help` |
| `scripts/bootstrap-mk-common.sh`, the make-common `.mk` files | never by hand: port the change to make-common, then `make mk-common-update` and `make help` |
| `scripts/check-license-headers.sh` | run it, and see a file without the header fail it |
| `scripts/check-suppressions.sh`, `.suppressions-baseline` | run it, and see an added `#[expect]` without a baseline change fail it |
| `meta/**` | `cargo test -p meta` |
| `**/*.md`, `.agents/skills/**` | `cargo test -p meta --test docs`; prose wraps at 100 columns |

## Project skills

Skills live in `.agents/skills/` (symlinked as `.claude/skills`). Load `adversarial-review` for any
review request ("review my diff", "is this ready to merge").
