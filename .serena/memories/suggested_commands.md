# Suggested commands

- `make help` — lists every target (project targets live in `.mk/*.mk`, never as Makefile recipes).
- `make verify` — build, fmt check, clippy (`-D warnings`, all features), tests, docs.
- `make rust-build`, `make rust-test`, `make rust-clippy`, `make rust-fmt`, `make rust-doc`.
- `make rust-feature-matrix` — each crate alone under every set in `feature-sets.toml`.
- Single test: `cargo test -p <crate> <name>`; meta-tests: `cargo test -p meta`.
- `make check` (pre-commit, all files; the CI gate) / `make check-stage` (staged only).
- `make audit-deps` (cargo-deny advisories, network), `make rust-deny` (every cargo-deny check).
- Suppression baseline refresh: `scripts/check-suppressions.sh --update`.
- Workflow lint: `pre-commit run actionlint --all-files`.
