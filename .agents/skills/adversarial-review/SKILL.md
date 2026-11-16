---
name: adversarial-review
description: >
  Adversarial code review of a set of changes to rust-common, the workspace of library crates
  the leinardi/* Rust projects share — working tree, staged diff, a branch vs main, a commit
  range, or a PR. Hunts for behaviour that changes for one consumer to suit another, extracted
  code that lost a fix or its provenance, public API and feature breaks, panics and lint
  escapes in library code, feature sets that compile only by unification, meta-tests that pass
  on nothing, workflow permission and pinning problems, and commit types that ship the wrong
  release, then reports ranked findings. Use when the user asks to review changes/a
  diff/a PR/a branch, "check my work before committing", "is this ready to merge", or "poke
  holes in this".
---

# Adversarial Review — rust-common

You are a hostile reviewer. Assume the change is **wrong until proven right** and find the
input, state or event where it breaks. A review that finds nothing is only credible after you
tried to break it and failed. Read `AGENTS.md` first: its conventions are the review's floor.
Then read `README.md`: it is the user-facing contract a change must keep true.

## Scope

When invoked directly: review the scope the user gives (`gh pr diff <n>` for a PR); with none,
the uncommitted work (`git diff HEAD` plus untracked files); on a clean tree, `git diff
main...HEAD`. Read every changed file in full, the files that call or document it, and the
stated intent (`git log --oneline` for the range): the change is checked against it.

## Repository invariants

### Library crates

- **A difference between consumers is a parameter, never a fork.** The crates replace copies
  that several projects ran with different behaviour on purpose (an exposition version, a log
  style, a timeout, a user agent). A change that alters what one consumer observes to suit
  another, or a default that silently picks one project's behaviour, is **high**: name the
  consumer and the output that changes. A second copy of the same logic for another consumer is
  a finding too.
- **Extraction keeps every fix and names its origin.** An extracted file carries
  `Extracted from <repo>/<path> at <sha>` in its module doc and the dual-license header in place
  of its original one. Diff it against the copies it replaces: a fix present in one copy and
  missing here is a regression for that consumer. Tests and fixtures move with the code, and
  byte-for-byte fixtures (`testdata/`) are moved, never re-generated or tidied.
- **Public API is the contract.** A changed `pub` signature, a removed item, a renamed feature
  or a new required parameter breaks consumers pinned to a tag: it needs a `!` (*Commits and
  releases* below) and a migration note in the commit body.
- **Features compile alone.** Each crate is built and tested on its own under every set in
  `feature-sets.toml` (`make rust-feature-matrix`). A `#[cfg(feature = ...)]` that names an
  undeclared feature, a feature that compiles only because another member enables a
  dependency feature, or a new feature with no set naming it is a finding.
- **Library hygiene.** No `unwrap`/`expect`, `print!`/`eprint!` or panic on input outside tests;
  no wall-clock read the caller cannot inject; no `unsafe` (`unsafe_code = "forbid"`); lints
  silenced only by `#[expect(lint, reason = "...")]`. A new
  dependency uses `default-features = false` where the defaults pull in more, and its licence is
  in `deny.toml`'s allow list for a stated reason.
- **Dependencies run downward.** A crate depends only on crates above it in the README's table.

### Meta-tests must not pass on nothing

`meta/tests/` guards claims nothing else checks: the workspace lints (`workspace_lints.rs`), the
feature-set coverage (`feature_sets.rs`). Make the guarded thing go wrong and see whether the test
still notices. A test that passes because its walk found no file, its pattern matched nothing or its
input was empty is a false pass; every rule needs a negative control over a scratch tree. A change
that loosens one (a narrower walk, a dropped negative control) is a finding unless it says why.

### Workflows

- Every action and reusable workflow is pinned by full commit SHA, with the version in a
  trailing comment that matches the SHA. A tag or branch ref is a finding.
- Top-level `permissions: contents: read`; a job that needs more says so on the job, and no
  more than it uses. A write permission on a job that runs pull-request code is **critical**.
- No `${{ }}` expression inlined in a `run:` script or a `github-script` body: it goes through
  `env:`. Inlined attacker-controlled values (titles, branch names, commit messages, inputs) are
  script injection, **critical**.
- A job gated on `github.event_name == 'pull_request'` must not be a required check that other
  events need to report.
- `ci-result` is the one required check. A gating job missing from its `needs` gates nothing; a
  result other than `success` may pass it only for `conventional-commits`, skipped off pull
  requests. `pre-commit-all` installs every tool its hooks need, and its `SKIP` lists only hooks a
  dedicated job runs.
- A release or other writing workflow refuses to run from anything but the default branch.
- Reason through every trigger a changed workflow has: pull request, push, dispatch, schedule.

### Shell and make

- Quoting: paths with spaces, empty variables, `$$` escaping in make recipes, a `\` continuation
  on every line of a multi-line recipe.
- Failure: `set -euo pipefail` in scripts; a step that exits 0 after a failed command (`;`
  instead of `&&`, a pipeline without `pipefail`, `|| true` that hides a real error).
- New targets live in a local `.mk/*.mk` listed in `MK_LOCAL_FILES`, never in the Makefile; a
  fetched `.mk` file edited by hand is lost on the next `make mk-common-update`.
- A change to `scripts/check-license-headers.sh` must still fail on a tracked file without the
  header.

### Commits and releases

Every commit is `type(scope): subject`, and the type decides the release. `svu` derives it from
the commits since the last tag, and every commit lands as it is (merge commits). A change users
should receive typed `chore`, `ci`, `docs`, `refactor` or `perf` never ships; a `feat` or `fix`
on CI, tests or docs cuts a release for nothing; a stray `!` or `BREAKING CHANGE:` cuts a major.
A `revert` of a shipped `feat` or `fix` is itself a `fix`. `svu` also matches `feat`/`fix`
anywhere before a colon (`prefix:` counts as `fix:`). See `AGENTS.md`.

## Adversarial passes

- **Defaults:** a default that changes what an existing user gets.
- **Empty and edge input:** unset optional inputs, a repository with no tags, an empty list, a
  first run, a re-run after a partial failure.
- **Contract drift:** README, `AGENTS.md`, `CONTRIBUTING.md` and the PR template still describe
  what the repository does.
- **A check that can no longer fail:** make the guarded thing go wrong and see whether the check
  still notices. A hook whose `files:` pattern matches nothing, a whole-tree hook that lost
  `always_run` or `stages`, a check that passes on an empty file list, a `pre-commit-all` that
  skips the hook, or a CI step with `|| true` or `continue-on-error` is a false pass.

## Gates

Do not re-run the repository's gates (`make check`, `make verify`, `make rust-feature-matrix`, `make
rust-deny`). The implementer is expected to run them and CI runs them again, but do not assume
either happened: report the verification evidence you actually have (under adversarial-review-loop,
`verification.txt` when supplied, which is the implementer's claim and not proof; when invoked
directly, the CI status or gate output in the PR or session) and what is missing, such as a change
to a crate's features or dependencies, or to `feature-sets.toml`, with no feature-matrix result
reported. Judge what is missing against `AGENTS.md`'s *Validation matrix*: every changed path owes
the checks its row names. Never say that a check you did not see passed. Judge whether the change's
tests prove it: a behaviour change needs a test that fails on the old code. When invoked directly,
one focused test (`cargo test -p <crate> <name>`) may be run to confirm a single suspected finding,
never a full suite or gate. A candidate that is neither reproduced nor traced end to end is dropped.

## Report

Rank worst first:

```text
<path>:<line> — <severity: critical | high | medium | low>: <one-line defect>
  Failure: <the concrete input/state → the wrong result or broken invariant>
  Fix: <the specific change>
```

End with a verdict (**block**, **approve with nits** or **approve**) and one line:
`Verification evidence: seen … / missing …`.
