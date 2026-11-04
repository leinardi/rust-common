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
# Extracted from swarm-scheduler-exporter/.mk/audit-deps.mk at c9e79afa8810; the scheduled-scan
# mention dropped, since this repository has no image to scan.

ifndef MK_LOCAL_AUDIT_DEPS_INCLUDED
MK_LOCAL_AUDIT_DEPS_INCLUDED := 1

# Local snippet (NOT part of make-common): check every Rust dependency against the RustSec
# advisory database, with cargo-deny as configured in deny.toml. It is not reachability-aware: any
# advisory against a crate in the graph counts, which is why every ignore in deny.toml carries a
# reason. It needs the network for the database, so it runs when Cargo.toml, Cargo.lock or
# deny.toml change (the cargo-deny hook, with every check), on every pull request (CI's rust job),
# and deliberately here. CARGO and RUST_DENY_ARGS (--workspace) come from make-common's rust.mk.

.PHONY: audit-deps
audit-deps: ## Check every Rust dependency against the RustSec advisories (cargo-deny; network required)
	$(CARGO) deny $(RUST_DENY_ARGS) check advisories

endif  # MK_LOCAL_AUDIT_DEPS_INCLUDED
