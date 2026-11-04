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
# Extracted from swarm-scheduler-exporter/.mk/verify.mk at c9e79afa8810; the integration-suite note
# dropped, since this workspace has none.

ifndef MK_LOCAL_VERIFY_INCLUDED
MK_LOCAL_VERIFY_INCLUDED := 1

# Local snippet (NOT part of make-common): one command that runs every check a change has to pass,
# the way CI's rust job does. verify-rust lives in make-common's rust.mk. The per-crate feature
# matrix is slower and has a target and a CI job of its own (rust-feature-matrix,
# .mk/feature-matrix.mk).

.PHONY: verify
verify: verify-rust ## Verify everything (build, fmt, clippy, tests, docs)

endif  # MK_LOCAL_VERIFY_INCLUDED
