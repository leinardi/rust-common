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

# Adapted from swarm-device-access/.mk/rust-release.mk at 4cd31c4, Apache-2.0, Copyright (c) 2026 Roberto Leinardi


ifndef MK_LOCAL_RUST_RELEASE_INCLUDED
MK_LOCAL_RUST_RELEASE_INCLUDED := 1

# Local snippet (NOT part of make-common): the static musl builds of the server, the release
# binaries release.yaml publishes (decisions.md section 27), and the pinned toolchain they need.

# The release toolchain beyond rust-toolchain.toml, each archive by its sha256: zig from
# https://ziglang.org/download/index.json, cargo-zigbuild from the release asset's .sha256, rust-std
# from the .sha256 static.rust-lang.org publishes beside each archive. This is the one record of
# those pins; the image's build stage (deployments/docker/Dockerfile) downloads the same archives,
# and meta/tests/toolchain.rs holds the two equal. cargo-zigbuild publishes no aarch64 musl asset, so an arm64 build host takes the
# gnu one. Only the image's build stage downloads the rust-std archives: on a host, rustup installs
# the musl targets rust-toolchain.toml lists.
RUST_RELEASE_ZIG_VERSION := 0.17.0
RUST_RELEASE_ZIG_SHA256_x86_64 := 1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026
RUST_RELEASE_ZIG_SHA256_aarch64 := 9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8
RUST_RELEASE_CARGO_ZIGBUILD_VERSION := 0.24.0
RUST_RELEASE_CARGO_ZIGBUILD_SHA256_x86_64-unknown-linux-musl := 52be2cb85ad3875682564a29bac1832b2ab3db68b1c504765c92d5249cc9b96f
RUST_RELEASE_CARGO_ZIGBUILD_SHA256_aarch64-unknown-linux-gnu := de42626985f428033078dfc12ac8a59ee713f8e5627235ec883b1c899bee895c
RUST_RELEASE_RUST_STD_VERSION := 1.98.1
RUST_RELEASE_RUST_STD_SHA256_x86_64-unknown-linux-musl := bd9f35880de21dab0e387e84009c00001ae7836a4ba565cb16823f13f75f325f
RUST_RELEASE_RUST_STD_SHA256_aarch64-unknown-linux-musl := 7e0ad2e67817f224b7a6331e48d6397303fc050edaccf81784ed786ffddc884a

# The release platforms (decisions.md section 27), as <docker arch>:<Rust target>, and where their
# binaries and checksums.txt go: dist/, under the names the release assets carry
# (bitgauge-linux-<arch>).
RUST_RELEASE_PLATFORMS := amd64:x86_64-unknown-linux-musl arm64:aarch64-unknown-linux-musl
RUST_RELEASE_DIR       := $(REPO_ROOT)/$(DIST_DIR)

# The one static build, for one Rust target: cargo-zigbuild with zig as the C compiler and linker,
# the version stamps of rust-dist, and the checkout and the cargo home remapped, so the binary
# carries no path of the machine that built it and a rebuild of the same commit gives the same
# bytes. The toolchain's rust-src, when it is installed, is mapped back to the /rustc/<commit> the
# standard library was built with: rustc otherwise writes the local copy's path, under the rustup
# home, in place of that prefix, so a machine with rust-src and one without would build different
# bytes. RUSTFLAGS is set, not inherited: a flag from the environment would change the shipped
# bytes. With --target, cargo applies it to the binary and not to build scripts. Recursive (=), so
# rustc is asked only by a recipe that builds. The SPA is whatever web/dist holds, so
# rust-release-binaries runs web-build first.
RUST_RELEASE_CARGO_HOME = $(or $(CARGO_HOME),$(HOME)/.cargo)
RUST_RELEASE_RUST_SRC   = $(shell cd "$(REPO_ROOT)" && rustc --print sysroot)/lib/rustlib/src/rust
RUST_RELEASE_RUSTC_HASH = $(shell cd "$(REPO_ROOT)" && rustc -vV | sed -n 's/^commit-hash: //p')
RUST_RELEASE_RUSTFLAGS  = --remap-path-prefix=$(REPO_ROOT)=/src \
  --remap-path-prefix=$(RUST_RELEASE_CARGO_HOME)=/cargo \
  --remap-path-prefix=$(RUST_RELEASE_RUST_SRC)=/rustc/$(RUST_RELEASE_RUSTC_HASH)
# $(call rust_static_build,<Rust target>)
rust_static_build = RUSTFLAGS="$(RUST_RELEASE_RUSTFLAGS)" $(RUST_VERSION_ENV) \
  $(CARGO) zigbuild --release --locked --target $(1) --bin $(BIN_NAME)

# The static builds take only the pinned zig and cargo-zigbuild: another release links other bytes.
.PHONY: rust-zigbuild-check
rust-zigbuild-check: ## Fail unless the zig and cargo-zigbuild on PATH are the pinned releases
	@set -eu; \
	for tool in zig cargo-zigbuild; do \
	  if ! command -v "$$tool" >/dev/null 2>&1; then \
	    echo "$$tool is not on PATH; install the release .mk/rust-release.mk pins" >&2; \
	    exit 1; \
	  fi; \
	done; \
	zig_version="$$(zig version)"; \
	if [ "$$zig_version" != "$(RUST_RELEASE_ZIG_VERSION)" ]; then \
	  echo "zig $$zig_version is on PATH; the static builds need zig $(RUST_RELEASE_ZIG_VERSION)" >&2; \
	  exit 1; \
	fi; \
	zigbuild_version="$$(cargo-zigbuild --version)"; \
	if [ "$$zigbuild_version" != "cargo-zigbuild $(RUST_RELEASE_CARGO_ZIGBUILD_VERSION)" ]; then \
	  echo "$$zigbuild_version is on PATH; the static builds need cargo-zigbuild $(RUST_RELEASE_CARGO_ZIGBUILD_VERSION)" >&2; \
	  exit 1; \
	fi

.PHONY: rust-release-binaries
rust-release-binaries: rust-zigbuild-check web-build ## Build the static musl Rust binaries (amd64, arm64) and checksums.txt into dist/
	set -eu; \
	mkdir -p "$(RUST_RELEASE_DIR)"; \
	for platform in $(RUST_RELEASE_PLATFORMS); do \
	  arch="$${platform%%:*}"; \
	  triple="$${platform#*:}"; \
	  $(call rust_static_build,"$$triple"); \
	  cp "$(REPO_ROOT)/target/$$triple/release/$(BIN_NAME)" "$(RUST_RELEASE_DIR)/$(BIN_NAME)-linux-$$arch"; \
	done; \
	cd "$(RUST_RELEASE_DIR)"; \
	sha256sum $(foreach platform,$(RUST_RELEASE_PLATFORMS),$(BIN_NAME)-linux-$(firstword $(subst :, ,$(platform)))) > checksums.txt

# Installs the pinned zig and cargo-zigbuild for the build host into RUST_RELEASE_TOOLS_DIR, each
# archive checked against its sha256 above before it is unpacked, for CI's rust-static job and the
# release workflow; put $(RUST_RELEASE_TOOLS_DIR)/bin on PATH afterwards. The directory must not
# exist yet, so nothing from an earlier install mixes in. cargo-zigbuild publishes no aarch64 musl
# asset, so an arm64 host takes the gnu one, as the image's build stage does.
RUST_RELEASE_TOOLS_DIR ?= $(REPO_ROOT)/target/zigbuild-tools

.PHONY: rust-zigbuild-install
rust-zigbuild-install: ## Install the pinned zig and cargo-zigbuild into RUST_RELEASE_TOOLS_DIR (network)
	@set -eu; \
	case "$$(uname -m)" in \
	  x86_64) zig_arch=x86_64; zig_sha256=$(RUST_RELEASE_ZIG_SHA256_x86_64); \
	    zigbuild_asset=x86_64-unknown-linux-musl; \
	    zigbuild_sha256=$(RUST_RELEASE_CARGO_ZIGBUILD_SHA256_x86_64-unknown-linux-musl) ;; \
	  aarch64) zig_arch=aarch64; zig_sha256=$(RUST_RELEASE_ZIG_SHA256_aarch64); \
	    zigbuild_asset=aarch64-unknown-linux-gnu; \
	    zigbuild_sha256=$(RUST_RELEASE_CARGO_ZIGBUILD_SHA256_aarch64-unknown-linux-gnu) ;; \
	  *) echo "no pinned zig for $$(uname -m)" >&2; exit 1 ;; \
	esac; \
	dir="$(RUST_RELEASE_TOOLS_DIR)"; \
	if [ -e "$$dir" ]; then echo "$$dir already exists; install into a new directory" >&2; exit 1; fi; \
	mkdir -p "$$dir/bin" "$$dir/zig" "$$dir/downloads"; \
	curl -fsSL --proto '=https' --tlsv1.2 -o "$$dir/downloads/zig.tar.xz" \
	  "https://ziglang.org/download/$(RUST_RELEASE_ZIG_VERSION)/zig-$$zig_arch-linux-$(RUST_RELEASE_ZIG_VERSION).tar.xz"; \
	echo "$$zig_sha256  $$dir/downloads/zig.tar.xz" | sha256sum -c -; \
	tar -xJf "$$dir/downloads/zig.tar.xz" -C "$$dir/zig" --strip-components=1; \
	ln -s ../zig/zig "$$dir/bin/zig"; \
	curl -fsSL --proto '=https' --tlsv1.2 -o "$$dir/downloads/cargo-zigbuild.tar.xz" \
	  "https://github.com/rust-cross/cargo-zigbuild/releases/download/v$(RUST_RELEASE_CARGO_ZIGBUILD_VERSION)/cargo-zigbuild-$$zigbuild_asset.tar.xz"; \
	echo "$$zigbuild_sha256  $$dir/downloads/cargo-zigbuild.tar.xz" | sha256sum -c -; \
	tar -xJf "$$dir/downloads/cargo-zigbuild.tar.xz" -C "$$dir/bin" --strip-components=1; \
	"$$dir/bin/zig" version; "$$dir/bin/cargo-zigbuild" --version

endif  # MK_LOCAL_RUST_RELEASE_INCLUDED
