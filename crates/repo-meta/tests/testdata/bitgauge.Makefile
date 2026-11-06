#
# This file is part of BitGauge.
#
# Copyright (c) 2026 Roberto Leinardi
#
# BitGauge is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# BitGauge is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with BitGauge.  If not, see <http://www.gnu.org/licenses/>.
#

# Resolve repository root (Makefile can live anywhere)
REPO_ROOT := $(shell git rev-parse --show-toplevel 2>/dev/null || pwd)

MK_COMMON_REPO    ?= leinardi/make-common
MK_COMMON_VERSION ?= v1

MK_COMMON_DIR := $(REPO_ROOT)/.mk

# Shared snippets coming from make-common
MK_COMMON_FILES := docker.mk help.mk pre-commit.mk web.mk parity.mk rust.mk

# Repo-local snippets that are NOT in make-common
MK_LOCAL_FILES := run.mk e2e.mk verify.mk rust-release.mk audit-deps.mk docker-run.mk image-test.mk harness.mk idle-measure.mk

MK_COMMON_BOOTSTRAP_SCRIPT := $(REPO_ROOT)/scripts/bootstrap-mk-common.sh

# Bootstrap: the script will self-update and fetch the selected .mk snippets
MK_COMMON_BOOTSTRAP := $(shell "$(MK_COMMON_BOOTSTRAP_SCRIPT)" \
  "$(MK_COMMON_REPO)" \
  "$(MK_COMMON_VERSION)" \
  "$(MK_COMMON_DIR)" \
  "$(MK_COMMON_FILES)")

# -----------------------------------------------------------------------------
# Project-specific config
# -----------------------------------------------------------------------------
BIN_NAME ?= bitgauge
DIST_DIR ?= dist

# Stamped the way release.yaml stamps a release: the version is the tag without its v only when
# HEAD is exactly a tag and the tree is clean, and otherwise dev, what an unstamped binary reports
# (src/buildinfo.rs); the commit is a fixed 7-character prefix; the date is the build's, in UTC. A
# local build's About screen then reads "dev · 34c88f5 · <date>". rust.mk hands the three to the
# binary as BITGAUGE_VERSION, BITGAUGE_COMMIT and BITGAUGE_DATE (RUST_STAMP_PREFIX), docker.mk to
# the image.
GIT_VERSION ?= $(or $(patsubst v%,%,$(filter-out %-dirty,$(shell git describe --tags --exact-match --dirty 2>/dev/null))),dev)
GIT_COMMIT  ?= $(or $(shell git rev-parse HEAD 2>/dev/null | cut -c1-7),none)
BUILD_DATE  ?= $(shell date -u +"%Y-%m-%dT%H:%M:%SZ")
RUST_STAMP_PREFIX := BITGAUGE

# The release binary make build leaves (web.mk's web-build, then rust.mk's rust-dist), the one
# make e2e, make run and the end-to-end fixtures (web/e2e/fixtures/server.ts) start.
DIST_BIN := $(REPO_ROOT)/$(DIST_DIR)/$(BIN_NAME)

# Docker image coordinates: one image, built from the shipped Dockerfile.
IMAGE_REPO     ?= ghcr.io/leinardi/bitgauge
IMAGE_TAG      ?= $(GIT_VERSION)
DOCKER_CONTEXT ?= .

DOCKER_TARGETS      := bitgauge
IMAGE_NAME_bitgauge := $(IMAGE_REPO)
DOCKERFILE_bitgauge := deployments/docker/Dockerfile

# --load puts the host-platform image into the local daemon, so docker-run and the image tests can
# run what docker-build produced.
DOCKER_BUILD_CMD ?= docker buildx build --load

# The design-system page make demo-parity-servers points at.
PARITY_PROTOTYPE_PAGE := ui_kits/app/index.html

# The parity servers' ports, off runhold's 5180 and 5181 so both projects can run the harness at
# once. A single run moves them with PARITY_DEMO_PORT=... PARITY_PROTO_PORT=... on the command line.
PARITY_DEMO_PORT ?= 5182
PARITY_PROTO_PORT ?= 5183

# The arguments `make run` hands to serve.
ARGS ?=

# -----------------------------------------------------------------------------
# Include shared make logic (fetched from make-common)
# -----------------------------------------------------------------------------
include $(addprefix $(MK_COMMON_DIR)/,$(MK_COMMON_FILES))

# -----------------------------------------------------------------------------
# Include repo-local logic (no bootstrap; lives only in this repo)
# -----------------------------------------------------------------------------
-include $(addprefix $(REPO_ROOT)/.mk/,$(MK_LOCAL_FILES))

.PHONY: mk-common-update
mk-common-update: ## Check for remote updates of shared .mk files
	@echo "[mk] Checking for updates from $(MK_COMMON_REPO)@$(MK_COMMON_VERSION)"
	MK_COMMON_UPDATE=1 "$(MK_COMMON_BOOTSTRAP_SCRIPT)" \
	  "$(MK_COMMON_REPO)" \
	  "$(MK_COMMON_VERSION)" \
	  "$(MK_COMMON_DIR)" \
	  "$(MK_COMMON_FILES)"

# -----------------------------------------------------------------------------
# Adding new targets
# -----------------------------------------------------------------------------
# Do NOT add recipes directly to this file. Instead:
#   - Project-specific targets → new .mk/<fragment>.mk added to MK_LOCAL_FILES
#   - Generic targets (useful beyond this repo) → new or updated .mk/<fragment>.mk
#     added to MK_COMMON_FILES, then open a PR to port the change upstream at
#     https://github.com/leinardi/make-common so mk-common-update keeps working.
