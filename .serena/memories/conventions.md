# Conventions (beyond AGENTS.md)

Lint set: `[workspace.lints]` in the root Cargo.toml (pedantic plus denies), every member opts in
with `[lints] workspace = true`, clippy runs workspace-wide with `--all-features`. Traps that
repeatedly bite:

- `min_ident_chars`: no single-letter names, closures included.
- `wildcard_enum_match_arm`: name every variant, no `_ =>` on an enum.
- `unwrap_used`/`expect_used` denied outside `#[test]`/`#[cfg(test)]`; helpers in `tests/*.rs`
  outside a `#[test]` are not covered.
- `print_stdout`/`print_stderr` denied: a library writes to the handle it is given (`writeln!`).
- No `disallowed_methods` yet: `clippy.toml` holds no application bans; adding one means adding the
  lint deny too.

Suppressions: only `#[expect(lint, reason = "...")]`, ratcheted per lint against
`.suppressions-baseline`; a commit adding, moving or removing one runs
`scripts/check-suppressions.sh --update` and commits the baseline. The reason says why the fix
does not apply, not which lint fired.

Extraction: copy the best consumer copy, merge the other copies' fixes, keep its tests and
fixtures, replace its licence header with the MIT OR Apache-2.0 one, add the `Extracted from`
line. A behaviour two consumers need differently becomes a parameter or a feature.

Features: every crate feature appears in a `feature-sets.toml` set; each crate has
`{ no-default-features = true }` and `{ all-features = true }` sets.

Tests: paused tokio clock for time, never sleep. Every `.rs` file carries the dual-licence
`/* */` header.

Commits: Conventional Commits with mandatory scope (the crate, or the area); type chosen by
whether it should ship (`fix`/`feat` release, others don't); avoid words ending in `fix`/`feat`
before a colon in other subjects (svu matches them).
