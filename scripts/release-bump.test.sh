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
# Tests scripts/release-bump.sh against throwaway Cargo workspaces with no dependencies (so
# `cargo metadata` needs no network), each in its own git repository.
#
# Run: make release-bump-test (needs cargo, jq and git)

set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-bump.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
export CARGO_TARGET_DIR="${WORK}/target" CARGO_NET_OFFLINE=true
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

PASS=0
FAIL=0
ok() {
	PASS=$((PASS + 1))
	echo "ok - $1"
}
not_ok() {
	FAIL=$((FAIL + 1))
	echo "not ok - $1"
	[[ -z "${2:-}" ]] || printf '%s\n' "$2" | sed 's/^/       /'
}

# new_repo NAME [B_VERSION_LINE]: a workspace of crates a and b at 1.0.0, committed. b inherits
# the version unless B_VERSION_LINE replaces its `version.workspace = true`.
new_repo() {
	local dir="${WORK}/$1" b_version="${2:-version.workspace = true}"
	mkdir -p "${dir}/a/src" "${dir}/b/src"
	cat >"${dir}/Cargo.toml" <<'TOML'
[workspace]
members = ["a", "b"]
resolver = "3"

[workspace.package]
version = "1.0.0"
edition = "2024"

[workspace.dependencies]
a = { path = "a", version = "1.0.0" }
TOML
	printf '[package]\nname = "a"\nversion.workspace = true\nedition.workspace = true\n' >"${dir}/a/Cargo.toml"
	printf '[package]\nname = "b"\n%s\nedition.workspace = true\n\n[dependencies]\na.workspace = true\n' "${b_version}" >"${dir}/b/Cargo.toml"
	: >"${dir}/a/src/lib.rs"
	: >"${dir}/b/src/lib.rs"
	git -C "${dir}" init -q
	(cd "${dir}" && cargo generate-lockfile -q)
	git -C "${dir}" add -A
	git -C "${dir}" -c user.name=t -c user.email=t@t commit -q -m init
	echo "${dir}"
}

# bump DIR VERSION: runs the script there; sets STATUS and LOG.
bump() {
	STATUS=0
	LOG="$(cd "$1" && bash "${SCRIPT}" "$2" 2>&1)" || STATUS=$?
}

repo="$(new_repo plain)"
bump "${repo}" 1.1.0
toml_diff="$(git -C "${repo}" diff -U0 Cargo.toml | grep -E '^[-+][^-+]' || true)"
if [[ ${STATUS} == 0 && "${toml_diff}" == $'-version = "1.0.0"\n+version = "1.1.0"' ]] &&
	grep -q 'name = "a"' "${repo}/Cargo.lock" &&
	[[ "$(git -C "${repo}" diff --name-only)" == $'Cargo.lock\nCargo.toml' ]]; then
	ok "a bump changes only the [workspace.package] version and the lock's workspace entries"
else
	not_ok "a bump changes only the [workspace.package] version and the lock's workspace entries" "${LOG}
${toml_diff}"
fi
if grep -q 'a = { path = "a", version = "1.0.0" }' "${repo}/Cargo.toml"; then
	ok "a version in another table is left alone"
else
	not_ok "a version in another table is left alone"
fi

for bad in 1.1 1.1.0.0 01.1.0 v1.1.0 1.1.0-rc.1 1.1.0+b ""; do
	repo="$(new_repo "bad-${bad//[^0-9a-z]/_}x")"
	bump "${repo}" "${bad}"
	if [[ ${STATUS} != 0 && -z "$(git -C "${repo}" status --porcelain)" ]]; then
		ok "'${bad}' is refused and nothing is written"
	else
		not_ok "'${bad}' is refused and nothing is written" "${LOG}"
	fi
done

repo="$(new_repo same)"
bump "${repo}" 1.0.0
if [[ ${STATUS} != 0 && "${LOG}" == *"expected Cargo.toml and Cargo.lock to change"* ]]; then
	ok "the version the crates already carry is refused"
else
	not_ok "the version the crates already carry is refused" "${LOG}"
fi

repo="$(new_repo two-lines)"
sed -i 's/^edition = "2024"$/edition = "2024"\nversion = "1.0.0"/' "${repo}/Cargo.toml"
git -C "${repo}" -c user.name=t -c user.email=t@t commit -q -am two
bump "${repo}" 1.1.0
if [[ ${STATUS} != 0 && "${LOG}" == *"has 2 version lines"* && -z "$(git -C "${repo}" status --porcelain)" ]]; then
	ok "two version lines in [workspace.package] stop before writing"
else
	not_ok "two version lines in [workspace.package] stop before writing" "${LOG}"
fi

repo="$(new_repo dirty)"
echo "# edit" >>"${repo}/Cargo.toml"
bump "${repo}" 1.1.0
if [[ ${STATUS} != 0 && "${LOG}" == *"uncommitted changes"* ]]; then
	ok "an uncommitted manifest change stops the bump"
else
	not_ok "an uncommitted manifest change stops the bump" "${LOG}"
fi

repo="$(new_repo own-version 'version = "0.5.0"')"
bump "${repo}" 1.1.0
if [[ ${STATUS} == 0 ]] && grep -q '^version = "0.5.0"$' "${repo}/b/Cargo.toml" &&
	[[ "$(cd "${repo}" && cargo metadata --format-version 1 --no-deps --locked | jq -r '[.packages[] | "\(.name)=\(.version)"] | sort | join(" ")')" == "a=1.1.0 b=0.5.0" ]]; then
	ok "a member with a version of its own keeps it"
else
	not_ok "a member with a version of its own keeps it" "${LOG}"
fi

repo="$(new_repo none-inherit 'version = "0.5.0"')"
sed -i 's/^version.workspace = true$/version = "0.4.0"/' "${repo}/a/Cargo.toml"
sed -i 's/^a = { path = "a", version = "1.0.0" }$/a = { path = "a", version = "0.4.0" }/' "${repo}/Cargo.toml"
(cd "${repo}" && cargo generate-lockfile -q)
git -C "${repo}" -c user.name=t -c user.email=t@t commit -q -am own
bump "${repo}" 1.1.0
if [[ ${STATUS} != 0 && "${LOG}" == *"no workspace member carries the [workspace.package] version 1.0.0"* && -z "$(git -C "${repo}" status --porcelain)" ]]; then
	ok "a workspace version no member inherits stops before writing"
else
	not_ok "a workspace version no member inherits stops before writing" "${LOG}"
fi

repo="$(new_repo other-file)"
echo "x" >"${repo}/untracked-but-added"
git -C "${repo}" add -N untracked-but-added
bump "${repo}" 1.1.0
if [[ ${STATUS} != 0 && "${LOG}" == *"expected Cargo.toml and Cargo.lock to change"* ]]; then
	ok "a change to any other file fails"
else
	not_ok "a change to any other file fails" "${LOG}"
fi

echo
echo "${PASS} passed, ${FAIL} failed"
[[ ${FAIL} -eq 0 ]]
