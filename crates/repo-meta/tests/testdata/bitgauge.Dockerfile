# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

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
# Adapted from swarm-scheduler-exporter/deployments/docker/Dockerfile at e57d989, MIT, Copyright (c) 2025 Roberto Leinardi
#
# The shipped image (build-packaging.md section 1). Every stage is a literal FROM, because
# Dependabot updates only literal FROM lines. All three bases come from dhi.io, so pulling them
# needs a dhi.io token.
#
# The dhi.io bases are pinned by an explicit tag, not by digest: dhi.io republishes its tags to ship
# security fixes, and a build should pick those up rather than wait for a Dependabot pull request.
# Every release build is scanned before anything is pushed, so a newer base never ships unscanned.
#
# The syntax line above pins the BuildKit frontend by index digest, so a moved 1.7 tag cannot change
# how this file is built. It is a parser directive and has to be the file's first line, ahead of
# this header. Dependabot's docker ecosystem does not update it: bump it by hand (docs/release.md).

########################
# web: build the SPA the binary embeds.
# Built here rather than copied from the build context's web/dist, so an image always carries a UI
# built from the same source tree as its binary. npm run build is vite build only: the type check
# is a CI gate, not a packaging step. Runs on the build platform; its output is architecture-free.
########################
FROM --platform=$BUILDPLATFORM dhi.io/node:24-alpine3.23-dev AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json web/.npmrc ./
RUN --mount=type=cache,target=/root/.npm,id=npmcache \
	npm ci --no-audit --no-fund
COPY web/ ./
# The design export's tokens, element defaults, fonts, icons and manifest, read in place by
# vite.config.ts.
COPY docs/design_system/colors_and_type.css docs/design_system/base.css /src/docs/design_system/
COPY docs/design_system/assets/ /src/docs/design_system/assets/
RUN npm run build

########################
# build: cross-compile a static musl binary for the target platform on the build platform, with
# cargo-zigbuild and zig as the C compiler and linker.
#
# The debian variant, because it carries the upstream toolchain, for which the upstream musl
# standard libraries below fit; the alpine variant's rustc is Alpine's own build for its
# x86_64-alpine-linux-musl host and cannot take another target's standard library. The tag's Rust
# release is rust-toolchain.toml's, and the pins are .mk/rust-release.mk's.
########################
FROM --platform=$BUILDPLATFORM dhi.io/rust:1.98.1-debian13-dev AS build
ARG VERSION="dev"
ARG COMMIT="none"
ARG DATE="unknown"

# The image's own toolchain version: the standard libraries added below must be the same release.
ARG RUST_VERSION=1.98.1
ARG ZIG_VERSION=0.17.0
ARG CARGO_ZIGBUILD_VERSION=0.24.0

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# curl and xz fetch and unpack zig and the standard libraries; nothing from this stage but the
# binary ships. Unpinned: dhi.io republishes this tag with a refreshed package index, which would
# break any pinned version, and every download below is checked by its own sha256.
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
	&& rm -rf /var/lib/apt/lists/*

# zig for the build platform, checked against the sha256 ziglang.org publishes.
ARG BUILDARCH
RUN case "${BUILDARCH}" in \
		amd64) zig_arch=x86_64; zig_sha256=1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026 ;; \
		arm64) zig_arch=aarch64; zig_sha256=9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8 ;; \
		*) echo "no zig for build platform ${BUILDARCH}" >&2; exit 1 ;; \
	esac \
	&& curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz" \
	&& echo "${zig_sha256}  /tmp/zig.tar.xz" | sha256sum -c - \
	&& mkdir /opt/zig \
	&& tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1 \
	&& rm /tmp/zig.tar.xz
ENV PATH="/opt/zig:${PATH}"

# cargo-zigbuild's release binary for the build platform, checked against the sha256 its release
# publishes. The release has no aarch64 musl asset, and this stage is glibc-based, so arm64 takes
# the gnu one.
RUN case "${BUILDARCH}" in \
		amd64) zigbuild_asset=x86_64-unknown-linux-musl; zigbuild_sha256=52be2cb85ad3875682564a29bac1832b2ab3db68b1c504765c92d5249cc9b96f ;; \
		arm64) zigbuild_asset=aarch64-unknown-linux-gnu; zigbuild_sha256=de42626985f428033078dfc12ac8a59ee713f8e5627235ec883b1c899bee895c ;; \
		*) echo "no cargo-zigbuild for build platform ${BUILDARCH}" >&2; exit 1 ;; \
	esac \
	&& curl -fsSL -o /tmp/cargo-zigbuild.tar.xz "https://github.com/rust-cross/cargo-zigbuild/releases/download/v${CARGO_ZIGBUILD_VERSION}/cargo-zigbuild-${zigbuild_asset}.tar.xz" \
	&& echo "${zigbuild_sha256}  /tmp/cargo-zigbuild.tar.xz" | sha256sum -c - \
	&& mkdir /tmp/cargo-zigbuild \
	&& tar -xJf /tmp/cargo-zigbuild.tar.xz -C /tmp/cargo-zigbuild --strip-components=1 \
	&& install -m 0755 /tmp/cargo-zigbuild/cargo-zigbuild /usr/local/bin/cargo-zigbuild \
	&& rm -rf /tmp/cargo-zigbuild /tmp/cargo-zigbuild.tar.xz \
	&& cargo-zigbuild --version

# The musl standard library of the target platform, checked against static.rust-lang.org's sha256.
ARG TARGETARCH
RUN case "${TARGETARCH}" in \
		amd64) triple=x86_64-unknown-linux-musl; std_sha256=bd9f35880de21dab0e387e84009c00001ae7836a4ba565cb16823f13f75f325f ;; \
		arm64) triple=aarch64-unknown-linux-musl; std_sha256=7e0ad2e67817f224b7a6331e48d6397303fc050edaccf81784ed786ffddc884a ;; \
		*) echo "no Rust target for platform ${TARGETARCH}" >&2; exit 1 ;; \
	esac \
	&& curl -fsSL -o /tmp/rust-std.tar.xz "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION}-${triple}.tar.xz" \
	&& echo "${std_sha256}  /tmp/rust-std.tar.xz" | sha256sum -c - \
	&& mkdir /tmp/rust-std \
	&& tar -xJf /tmp/rust-std.tar.xz -C /tmp/rust-std --strip-components=1 \
	&& /tmp/rust-std/install.sh --prefix=/usr/local --disable-ldconfig \
	&& rm -rf /tmp/rust-std /tmp/rust-std.tar.xz \
	&& echo "${triple}" > /tmp/triple

WORKDIR /src

# The source, then the SPA the web stage built; the build context carries no web/dist of its own,
# and build.rs embeds whatever web/dist holds.
COPY . .
COPY --from=web /src/web/dist ./web/dist

# The registry and the target directory are BuildKit caches, so the binary is copied out of them.
# The registry cache is shared by both platforms' builds, which run in parallel: sharing=locked
# serializes them, since two cargo processes unpacking the same crate into it fail with
# "File exists". The target cache is per platform and needs no lock. The version stamps are the
# ones make rust-dist sets, and the paths are remapped as .mk/rust-release.mk remaps them: the
# checkout is /src already, the cargo home becomes /cargo, and the toolchain's rust-src, when the
# image has it, the /rustc/<commit> the standard library was built with. RUSTFLAGS is set, not
# inherited, and with --target cargo applies it to the binary and not to build scripts.
RUN --mount=type=cache,target=/root/.cargo/registry,id=cargo-registry,sharing=locked \
	--mount=type=cache,target=/src/target,id=bitgauge-cargo-target-${TARGETARCH} \
	triple="$(cat /tmp/triple)" \
	&& rust_src="$(rustc --print sysroot)/lib/rustlib/src/rust" \
	&& rustc_hash="$(rustc -vV | sed -n 's/^commit-hash: //p')" \
	&& RUSTFLAGS="--remap-path-prefix=/root/.cargo=/cargo --remap-path-prefix=${rust_src}=/rustc/${rustc_hash}" \
		BITGAUGE_VERSION="${VERSION}" \
		BITGAUGE_COMMIT="${COMMIT}" \
		BITGAUGE_DATE="${DATE}" \
		cargo zigbuild --release --locked --target "${triple}" --bin bitgauge \
	&& mkdir -p /out \
	&& cp "target/${triple}/release/bitgauge" /out/bitgauge

########################
# runtime: the shell-less dhi.io static base. The binary is linked statically against musl, so it
# needs no libc, and it writes nothing: the image declares no volume and runs on a read-only root
# filesystem.
########################
FROM dhi.io/static:20250419 AS runtime
ARG VERSION="dev"
ARG COMMIT="none"
ARG DATE="unknown"

LABEL org.opencontainers.image.title="bitgauge" \
	org.opencontainers.image.description="Self-hosted, browser-based network speed test for LAN, homelab and VPN links" \
	org.opencontainers.image.source="https://github.com/leinardi/bitgauge" \
	org.opencontainers.image.version="${VERSION}" \
	org.opencontainers.image.revision="${COMMIT}" \
	org.opencontainers.image.created="${DATE}" \
	org.opencontainers.image.licenses="GPL-3.0-or-later"

COPY --from=build /out/bitgauge /bitgauge

# Spelled out rather than inherited: the base's default user is this same unprivileged identity,
# but the image's posture must not depend on a base image's default.
USER 65532:65532

EXPOSE 8080

# Exec form: there is no shell. The subcommand loads the same configuration as serve and probes the
# server's own listen address and base path (backend.md section 6), so a deployment that changes
# either needs no override. The timings are build-packaging.md section 1's: the probe's own request
# times out after 3 s, so the 5 s timeout never cuts a probe that would have answered; startup is
# config validation, a 4 MiB random buffer and a listen, which the 5 s start period covers; three
# failures 30 s apart mark a stuck server unhealthy without flapping on one slow probe.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
	CMD ["/bitgauge", "healthcheck"]

ENTRYPOINT ["/bitgauge"]
