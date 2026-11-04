<!-- markdownlint-disable MD041 -->
<!--
Any HTML comment will be stripped when the markdown is rendered, so you don't need to delete them.
-->

## Summary

<!-- Short description of what this PR does. -->

---

## Pull request checklist

- [ ] I am targeting the `main` branch
- [ ] I have **rebased** this branch on top of the destination branch
- [ ] I have executed `make verify` and `make check` locally *before creating the commit* and they
  have run successfully
- [ ] Every commit message follows
  [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) with a scope
  (`type(scope): subject`, the scope being the crate or the area), and its type matches whether the
  change ships (`feat`/`fix` only for what the consuming projects should receive in a release)
- [ ] I have performed a self-review of my own code
- [ ] There are no `WIP` commits in this PR
- [ ] I ran the validation commands for every area touched (see the AGENTS.md validation matrix)
- [ ] An extracted file names its origin (`Extracted from <repo>/<path> at <sha>`), and a
  behaviour two consumers need differently is a parameter, not a copy

---

## Type of changes

<!-- Tick all that apply: -->

- [ ] 🐛 Bug fix
- [ ] ✨ New feature
- [ ] 🔧 Refactoring
- [ ] 📜 Docs
- [ ] 🧰 CI / tooling / infra
- [ ] Other (describe in Summary)
