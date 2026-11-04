# Task completion

Run the rows of the AGENTS.md validation matrix that match the edited paths; `make check` applies to
every row. Most common:

- `**/*.rs`, `Cargo.toml`, `Cargo.lock`: `make verify`.
- A crate's features or dependencies, `feature-sets.toml`: `make rust-feature-matrix`.
- `Cargo.toml`/`Cargo.lock`/`deny.toml`: `make rust-deny` (network).
- `.github/**`: `pre-commit run actionlint --all-files`.
- `meta/**`: `cargo test -p meta`.
- `**/*.md`: `cargo test -p meta --test docs`; prose wraps at 100 columns.
- New file: MIT OR Apache-2.0 header with the year of first commit
  (`scripts/check-license-headers.sh`); an extracted file also names its origin.
- New files must be staged before `make check`: pre-commit skips untracked files.
