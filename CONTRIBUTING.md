# Contributing

## Prerequisites

- Rust 1.98: [rustup](https://rustup.rs/) installs the `1.98.1` toolchain that
  `rust-toolchain.toml` pins, with clippy and rustfmt, on the first `cargo` command
- [cargo-deny](https://github.com/EmbarkStudios/cargo-deny), for `make rust-deny`,
  `make audit-deps` and the `cargo-deny` hook (`cargo install --locked cargo-deny`)
- [`pre-commit`](https://pre-commit.com/): install the hooks once with `make pre-commit-install`. It
  installs both the `pre-commit` and the `commit-msg` hooks, so commit messages are checked when you
  commit. The hooks bring their own tools, shellcheck included; actionlint also checks the
  workflows' `run:` scripts when a [`shellcheck`](https://www.shellcheck.net/) is on `PATH`, as CI
  installs it (`.github/actions/setup-shellcheck`).

## Building and testing

```bash
make verify               # rust-build, rust-fmt-check, rust-clippy, rust-test and rust-doc in one go
make rust-test            # cargo test --workspace, the meta-tests in meta/ included
make rust-fmt             # format the code (rust-fmt-check only checks)
make rust-feature-matrix  # each crate on its own under every set in feature-sets.toml
make rust-deny            # every cargo-deny check: advisories, licences, bans, sources (network required)
make check                # the full pre-commit suite on every file
make check-stage          # the pre-commit suite on the staged files only
make help                 # every target
```

Read [AGENTS.md](AGENTS.md) for the layout and the rules a change must keep: where an extracted file
records its origin, why a difference between two consumers becomes a parameter, and the validation
matrix.

## Commit messages

All commits must follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/)
with a scope: `<type>(<scope>)[!]: <description>`. The scope is the crate or the area. The
`conventional-pre-commit` hook enforces this on `commit-msg`, and the `conventional-commits` CI job
checks it again on every pull request.

```text
fix(go-prom): snapshot the collector descriptors outside the lock
feat(docker-engine): add the attach feature
docs(readme): fix a link
```

The release version is derived from these types, since the last release:

| Release | Commit | Example |
| --- | --- | --- |
| major | any type with `!` before the colon, or a `BREAKING CHANGE:` footer | `feat(go-slog)!: take the level parser as a parameter` |
| minor | `feat` | `feat(docker-engine): add the attach feature` |
| patch | `fix` | `fix(go-http): enforce an earlier deadline at once` |
| none | everything else: `build`, `chore`, `ci`, `docs`, `perf`, `refactor`, `style`, `test`, `revert` | `docs(readme): fix a link` |

Pick the type by whether the change should ship in a release, not by what kind of change it is: a
change the consumers should receive when they move to the next tag is a `fix` or a `feat`. A
`revert` of a shipped `feat` or `fix` is itself a `fix`. The version tool matches `feat` and `fix`
anywhere before the colon (`prefix:` counts as `fix:`), so keep such words out of other subjects.

Pull requests are merged with merge commits; squash and rebase merging are disabled. Every commit
therefore lands on `main` as it is, so each one needs a correct type, not just the pull request as a
whole.

## Releasing

A release is two workflows and one merge, so a `vX.Y.Z` tag always marks a tree whose crates carry
version `X.Y.Z`:

1. Dispatch the **Release** workflow (`release.yaml`) from `main`. Leave the version empty to derive
   it from the commits since the last tag, or pass one (e.g. `0.3.0` or `v0.3.0`); tick *dry run*
   first to see what it would do. It refuses a version not above every release tag and above the
   crates' current version. It writes the version to `[workspace.package]` in the root
   `Cargo.toml` (every crate inherits it) and to the workspace entries of `Cargo.lock`, pushes a
   `release/vX.Y.Z` branch and opens a `chore(release): bump version to X.Y.Z` pull request, with
   CI dispatched on it.
2. Review and merge that pull request (a merge commit, as always).
3. **Auto Tag Release** (`auto-tag-release.yaml`) runs on the push to `main`, sees the version
   change, and calls
   [`simple-tag-and-release`](https://github.com/leinardi/gh-reusable-workflows/blob/main/.github/workflows/simple-tag-and-release.md)
   with it: the `vX.Y.Z` tag on the merge commit, the release with generated notes, and `vMAJOR`
   and `latest` moved when it is the highest release. A failed run is finished by re-running it.

- **The version is chosen at dispatch.** If `main` moves before the bump pull request is merged,
  close it and dispatch again, so the version counts the new commits.
- **A failed Release run** is finished by re-running it: a leftover `release/vX.Y.Z` branch without
  a pull request is recreated, and an open pull request gets its CI run again.
- **Never edit the version or push a tag by hand.** A version change merged to `main` by anything
  but the `release/vX.Y.Z` pull request fails Auto Tag Release instead of tagging.
- **v0.2.0 is tagged on crates that still say `0.1.0`** (it predates this flow), so its consumers
  write `version = "0.1"`. The next release needs an explicit version, `0.2.1` or above, since
  `0.2.0` is taken.

Then move each consumer to the new `tag`, with `version` set to the tag's `<major>.<minor>` (cargo
checks that requirement against the crates' version).
