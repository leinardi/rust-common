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
# The test tools image (testing.md section 5.1), never shipped: the netguard holder, the strace
# tracer of the image test's syscall layer, and the harness shaper and iperf3 endpoints all run it,
# and the shaper also runs the compatibility clients of the CI tier (testing.md section 5). The
# Alpine base is pinned by digest; the packages are not: apk installs the 3.23 branch's current
# builds, because Alpine drops superseded package versions from its index and an exact pin would
# stop building. A stable branch only takes fixes, and the tests assert behavior (nft counting,
# strace -yy annotations), not versions. strace must be 5 or later for -yy, which annotates socket
# descriptors with their endpoints.
#
# The two compatibility clients are built from source in a Go stage pinned by digest, each at an
# exact module version that the Go checksum database verifies: librespeed-cli at its release tag
# v1.0.14, and goresponsiveness at the pseudo-version of commit 343954285090 (2024-01-29), its
# main branch head, since the project publishes no release tags. test/dockerfiles pins both
# versions, and test/toolchain the Go line of the builder.

FROM docker.io/library/golang:1.26-alpine3.23@sha256:6750308561953d451210ed1a8086d36c8c8bdcbf672d38d9d0d424b8a7a0216c AS clients

ENV CGO_ENABLED=0 GOTOOLCHAIN=local GOBIN=/out

RUN go install -trimpath \
		-ldflags "-s -w -X github.com/librespeed/speedtest-cli/defs.ProgName=librespeed-cli -X github.com/librespeed/speedtest-cli/defs.ProgVersion=v1.0.14" \
		github.com/librespeed/speedtest-cli@v1.0.14 \
	&& mv /out/speedtest-cli /out/librespeed-cli \
	&& go install -trimpath -ldflags "-s -w" \
		github.com/network-quality/goresponsiveness@v0.0.0-20240129151524-343954285090

FROM docker.io/library/alpine:3.23@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0

LABEL org.opencontainers.image.title="bitgauge-test-tools" \
	org.opencontainers.image.description="BitGauge test tools: nftables, iproute2, iperf3, iputils, strace, librespeed-cli v1.0.14 and goresponsiveness v0.0.0-20240129151524-343954285090" \
	org.opencontainers.image.source="https://github.com/leinardi/bitgauge" \
	org.opencontainers.image.version="test" \
	org.opencontainers.image.revision="test" \
	org.opencontainers.image.created="test" \
	org.opencontainers.image.licenses="GPL-3.0-or-later"

RUN apk add --no-cache iproute2 iperf3 nftables iputils strace

COPY --from=clients /out/librespeed-cli /out/goresponsiveness /usr/local/bin/

CMD ["sleep", "infinity"]
