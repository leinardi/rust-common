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

ifndef MK_LOCAL_FEATURE_MATRIX_INCLUDED
MK_LOCAL_FEATURE_MATRIX_INCLUDED := 1

# Local snippet (NOT part of make-common): lint (clippy over every target, warnings as errors, so a
# set that compiles but leaves an unused import fails) and test every library crate under each
# feature set feature-sets.toml lists for it, one package at a time (--manifest-path
# crates/<name>/Cargo.toml -p <name>), so the features another workspace member enables cannot unify in and hide a missing
# feature declaration. The runner is the meta crate's feature-matrix binary, which reads the file
# with the same parser as the meta-test that checks every crate feature is covered
# (meta/tests/feature_sets.rs). It fails on a malformed file and on a listed crate whose manifest
# is missing; with no crate listed it says so and succeeds, which is the state before the first
# crate lands. CARGO comes from make-common's rust.mk.

.PHONY: rust-feature-matrix
rust-feature-matrix: ## Lint (-D warnings) and test each crate under every feature set in feature-sets.toml
	$(CARGO) run --locked --quiet -p meta --bin feature-matrix

endif  # MK_LOCAL_FEATURE_MATRIX_INCLUDED
