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

ifndef MK_LOCAL_RELEASE_INCLUDED
MK_LOCAL_RELEASE_INCLUDED := 1

# Local snippet (NOT part of make-common). release-bump is what the Release workflow
# (gh-reusable-workflows' version-bump-pr) runs with the version it resolved: it writes the crates'
# version into Cargo.toml and Cargo.lock and never commits. release-bump-test runs its tests
# against throwaway workspaces (CI's rust job runs them).

.PHONY: release-bump
release-bump: ## Write VERSION (X.Y.Z) into the crates' version and Cargo.lock, e.g. make release-bump VERSION=1.2.0
	@test -n "$(VERSION)" || { echo "usage: make release-bump VERSION=X.Y.Z" >&2; exit 2; }
	@"$(REPO_ROOT)/scripts/release-bump.sh" "$(VERSION)"

.PHONY: release-bump-test
release-bump-test: ## Test scripts/release-bump.sh against throwaway Cargo workspaces
	@bash "$(REPO_ROOT)/scripts/release-bump.test.sh"

endif
