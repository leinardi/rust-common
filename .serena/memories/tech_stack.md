# Tech stack

- Rust: one line (1.98) across rust-toolchain.toml (1.98.1), the workspace `rust-version`
  ("1.98", so consumers on any 1.98 patch build) and every CI toolchain step.
- Distribution: git tags `vX.Y.Z` from the Release workflow (gh-reusable-workflows
  `simple-tag-and-release`, svu from Conventional Commits); every crate shares the workspace
  version; consumers depend with `git` + `tag` + `version = "0.1"`.
- Lint and audit: clippy (workspace lints), rustfmt, cargo-machete, cargo-deny (`deny.toml`,
  crates.io only).
- Build/infra: Makefile + `.mk/*.mk`; make-common snippets (`.mk/{help,password,pre-commit,
  rust}.mk`) are vendored from `leinardi/make-common`: never hand-edit, refresh with
  `make mk-common-update`.
- Meta crate: regex (Markdown links), toml (manifests, `feature-sets.toml`), tempfile (scratch
  trees in negative controls).
