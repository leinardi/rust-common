#!/usr/bin/env bash
#
# Copyright (c) 2026 Roberto Leinardi
#
# Licensed under either of
#
#  * Apache License, Version 2.0 (LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0)
#  * MIT license (LICENSE-MIT or https://opensource.org/licenses/MIT)
#
# at your option.
#
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Writes a release version into the crates' package version: the one `version` line of
# `[workspace.package]` in Cargo.toml, which every member inherits with `version.workspace = true`,
# and the workspace entries of Cargo.lock. Called by the Release workflow (gh-reusable-workflows'
# version-bump-pr) as `make release-bump VERSION="$RELEASE_VERSION"`; it never commits.
#
# Usage: scripts/release-bump.sh X.Y.Z
#
# Only that line of Cargo.toml changes. A plain resolve (`cargo metadata`, without --locked) then
# rewrites the workspace entries of Cargo.lock and nothing else; `cargo update` would also move
# unrelated dependencies. It fails, after writing, when anything but Cargo.toml and Cargo.lock
# changed, when Cargo.lock gained a line other than a workspace entry's new version, or when a
# member that carried the old version does not carry the new one. A member with a version of its
# own (the internal meta crate's 0.0.0) keeps it.

set -euo pipefail

die() {
	echo "release-bump: $*" >&2
	exit 1
}

[[ $# -eq 1 ]] || die "usage: release-bump.sh X.Y.Z"
VERSION="$1"
[[ "${VERSION}" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] ||
	die "the version must be X.Y.Z with no leading zeros, no pre-release and no build metadata (got '${VERSION}')"
command -v cargo >/dev/null || die "cargo is not on PATH"
command -v jq >/dev/null || die "jq is not on PATH"

ROOT="$(git rev-parse --show-toplevel)"
cd "${ROOT}"
[[ -z "$(git status --porcelain -- Cargo.toml Cargo.lock)" ]] ||
	die "Cargo.toml or Cargo.lock has uncommitted changes; the bump must start from a clean manifest"

# The [workspace.package] table runs from its header to the next table header. Exactly one
# `version = "..."` line in it is rewritten; any other count stops before writing.
tmp="$(mktemp)"
trap 'rm -f "${tmp}"' EXIT
read -r count old < <(awk -v version="${VERSION}" '
	/^\[/ { in_table = ($0 == "[workspace.package]") }
	in_table && /^version = "[^"]*"$/ { old = $0; $0 = "version = \"" version "\""; n++ }
	{ print > out }
	END { gsub(/^version = "|"$/, "", old); print n + 0, old }
' out="${tmp}" Cargo.toml)
[[ "${count}" == 1 ]] || die "[workspace.package] in Cargo.toml has ${count} version lines, expected 1"

# The members at a version, by name: the ones at the table's version inherit it.
members() {
	cargo metadata --format-version 1 --no-deps --locked |
		jq -r --arg v "$1" '[.packages[] | select(.version == $v) | .name] | sort | join(" ")'
}
INHERITING="$(members "${old}")"
[[ -n "${INHERITING}" ]] || die "no workspace member carries the [workspace.package] version ${old}"

cat "${tmp}" >Cargo.toml

cargo metadata --format-version 1 >/dev/null
cargo metadata --format-version 1 --locked >/dev/null

changed="$(git diff --name-only)"
[[ "${changed}" == "$(printf 'Cargo.lock\nCargo.toml')" ]] ||
	die "expected Cargo.toml and Cargo.lock to change, got: ${changed//$'\n'/, }"
# Every line the resolve added to Cargo.lock is a workspace entry's new version.
stray="$(git diff -U0 Cargo.lock | awk -v want="+version = \"${VERSION}\"" '/^\+[^+]/ && $0 != want')"
[[ -z "${stray}" ]] || die "the resolve changed more than the workspace versions in Cargo.lock: ${stray}"
# Every member that carried the old version carries the new one.
now="$(members "${VERSION}")"
[[ "${now}" == "${INHERITING}" ]] || die "the members at ${old} were '${INHERITING}', the members at ${VERSION} are '${now}'"

git diff --stat
echo "release-bump: the crates are at ${VERSION}"
