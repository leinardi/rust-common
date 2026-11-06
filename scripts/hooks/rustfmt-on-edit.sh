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
# Extracted from swarm-scheduler-exporter/scripts/hooks/rustfmt-on-edit.sh at c9e79afa8810.
#
# rustfmt-on-edit.sh — Claude Code PostToolUse hook (configured in .claude/settings.json).
# Auto-formats Rust files immediately after an Edit/Write tool call so formatting
# drift is fixed at edit time instead of failing later at pre-commit.
#
# Input: hook payload JSON on stdin; the edited path is at .tool_input.file_path.
# rustfmt finds the repository's rustfmt.toml (edition, max_width) from the file's directory.
# Always exits 0 (a formatting fix must never block the agent).

set -euo pipefail

file="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("file_path",""))' 2>/dev/null || true)"

case "$file" in
  *.rs)
    if [[ -f "$file" ]] && command -v rustfmt >/dev/null 2>&1; then
      rustfmt --quiet "$file" || true
    fi
    ;;
esac

exit 0
