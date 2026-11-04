# rust-common — core

Cargo workspace of library crates shared by the leinardi/* Rust projects (swarm-scheduler-exporter,
swarm-device-access, bitgauge, awtrix-controller, swarm-rootless-docker); consumed by git tag
(`vX.Y.Z`), never published. Code is extracted from those projects, union of their fixes.

AGENTS.md (repo root) is the authoritative contract: layout, commands, rules for the crates,
validation matrix, commit types. Read it before any change; memories below only add what it does
not say. README is the consumer-facing contract (crate table, tag dependency, `[patch]`).

## Source map

- `crates/<name>/`: one library crate each (planned list in the README table); each is a workspace
  member and has a `feature-sets.toml` entry.
- `meta/`: meta-tests over the repository (`tests/workspace_lints.rs`, `docs.rs`,
  `workspace_layout.rs`, `feature_sets.rs`), the checks in `src/` (`repo`, `links`,
  `feature_sets`, `workspace`), and `src/bin/feature-matrix.rs`, the runner behind
  `make rust-feature-matrix`.
- `feature-sets.toml`: per-crate feature sets, format documented in the file.

## Non-obvious anchors

- Each extracted file names its origin: `Extracted from <repo>/<path> at <sha>` in the module doc.
- A consumer difference is a parameter, never a fork; per-project goldens stay in the consumers.
- `crates/*/tests/testdata/**` is byte-for-byte: never tidy.

Further: tooling and versions in `mem:tech_stack`; commands in `mem:suggested_commands`; Rust
style and lint traps in `mem:conventions`; done-criteria in `mem:task_completion`.
