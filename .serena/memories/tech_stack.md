# Tech stack

- Rust: one line (1.98) across rust-toolchain.toml (1.98.1), the workspace `rust-version`
  ("1.98", so consumers on any 1.98 patch build) and every CI toolchain step.
- Distribution: git tags `vX.Y.Z`. The Release workflow (svu from Conventional Commits) opens a
  pull request writing the workspace version; its merge makes Auto Tag Release tag it through
  gh-reusable-workflows' `simple-tag-and-release`. Every crate shares the workspace version;
  consumers depend with `git` + `tag` + `version` = the tag's `<major>.<minor>`.
- Lint and audit: clippy (workspace lints), rustfmt, cargo-machete, cargo-deny (`deny.toml`,
  crates.io only).
- Build/infra: Makefile + `.mk/*.mk`; make-common snippets (`.mk/{help,password,pre-commit,
  rust}.mk`) are vendored from `leinardi/make-common`: never hand-edit, refresh with
  `make mk-common-update`.
- Meta crate: regex (Markdown links), toml (manifests, `feature-sets.toml`), tempfile (scratch
  trees in negative controls).
