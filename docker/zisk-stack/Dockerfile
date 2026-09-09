# syntax=docker/dockerfile:1.7
# The ZiSK proving stack for ZKsync OS: three images from one file, selected
# with `--target`.
#
#   coordinator   zisk-coordinator from the pinned ZiSK toolchain tarball.
#                 CPU only. Owns the job queue and the client API.
#   worker        zisk-worker (GPU build). Holds the proving keys and the GPU
#                 for the service lifetime, so keys load once instead of once
#                 per proof. Also carries `zisk-prepare-keys`, the one-shot
#                 key fetch/verify/extract step, and cargo-zisk-dev for the
#                 constant-tree generation that step runs.
#   prover        the ZKsync OS daemon (zksync-os-zisk-prover-service), the
#                 two hash-verified guest ELFs, and the CPU cargo-zisk whose
#                 `remote` subcommands drive the coordinator. No GPU, no keys.
#   prover-export helper: builds the daemon from this checkout and exports the
#                 binary (`docker build --target prover-export -o out .`). The
#                 prover target COPYs out/zksync-os-zisk-prover-service from the
#                 build context, so a release build supplies the exact released
#                 binary instead of rebuilding it.
#
# Every ZiSK binary comes from the same pinned release tarball, verified by
# sha256 before extraction; nothing ZiSK is compiled here. The tarball is
# x86_64 only, so these images are linux/amd64.
#
# The proving keys are NOT in any image (26 GB compressed). They arrive at
# first start through `zisk-prepare-keys` into a volume the worker mounts; see
# README.md next to this file.
#
# Build from the repo root; the ELF pins and out/ live there:
#   docker/zisk-stack/build-images.sh --prover-from-docker

ARG ZISK_VERSION=1.2.0-alpha
ARG ZISK_TARBALL_SHA256=e3605dafb5fcdb2fa8271ea945334694f42704737f31ef0bdf93d663f2b14866
ARG UBUNTU_TAG=24.04

# ---------------------------------------------------------------------------
# The pinned ZiSK toolchain tarball, sha256-verified, unpacked once and copied
# into the images below binary by binary.
FROM ubuntu:${UBUNTU_TAG} AS toolchain
ARG ZISK_VERSION
ARG ZISK_TARBALL_SHA256
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL --retry 5 -o /tmp/cargo_zisk.tar.gz \
        "https://github.com/0xPolygonHermez/zisk/releases/download/v${ZISK_VERSION}/cargo_zisk_linux_amd64.tar.gz" \
    && echo "${ZISK_TARBALL_SHA256}  /tmp/cargo_zisk.tar.gz" | sha256sum -c - \
    && mkdir -p /opt/zisk-dist \
    && tar --no-same-owner -xzf /tmp/cargo_zisk.tar.gz -C /opt/zisk-dist \
    && rm /tmp/cargo_zisk.tar.gz \
    && ls -la /opt/zisk-dist/bin

# ---------------------------------------------------------------------------
# Shared runtime: the dynamic libraries every binary in the tarball links
# (libgmp, libgomp and libomp, OpenMPI, libsodium; the GPU builds add only the
# driver's libcuda.so.1, which the NVIDIA container toolkit mounts at run
# time), the zisk user with the uid/gid ZiSK's own images use, and the ZiSK
# path layout the binaries resolve through ZISK_HOME and ZISK_CACHE_DIR.
FROM ubuntu:${UBUNTU_TAG} AS runtime-base
ARG ZISK_VERSION
ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl \
        libgmp10 libgomp1 libomp5-18 libopenmpi3t64 libsodium23 \
    && rm -rf /var/lib/apt/lists/*
RUN groupadd --system --gid 999 zisk \
    && useradd --system --uid 998 --gid zisk --no-create-home \
               --shell /usr/sbin/nologin --home-dir /opt/zisk zisk \
    && mkdir -p /opt/zisk/bin /var/cache/zisk /etc/zisk \
    && chown -R zisk:zisk /opt/zisk /var/cache/zisk
ENV ZISK_HOME=/opt/zisk \
    ZISK_CACHE_DIR=/var/cache/zisk \
    ZISK_VERSION=${ZISK_VERSION} \
    PATH=/opt/zisk/bin:$PATH
LABEL org.opencontainers.image.source="https://github.com/matter-labs/zksync-os-zisk" \
      org.opencontainers.image.vendor="Matter Labs" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

# ---------------------------------------------------------------------------
FROM runtime-base AS coordinator
COPY --from=toolchain --chown=root:root --chmod=0755 \
     /opt/zisk-dist/bin/zisk-coordinator /opt/zisk/bin/zisk-coordinator
COPY --chown=root:root docker/zisk-stack/coordinator.toml docker/zisk-stack/coordinator-core.toml /etc/zisk/
# Client API | worker cluster port | Prometheus metrics and /health.
EXPOSE 7000 50051 9090
LABEL org.opencontainers.image.title="zksync-os-zisk-coordinator" \
      org.opencontainers.image.description="ZiSK coordinator for the ZKsync OS second proof system"
USER zisk
WORKDIR /var/cache/zisk
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9090/health || exit 1
ENTRYPOINT ["zisk-coordinator"]
CMD ["--config", "/etc/zisk/coordinator.toml"]

# ---------------------------------------------------------------------------
FROM runtime-base AS worker
ARG DEBIAN_FRONTEND=noninteractive
# The ASM emulator path assembles and links a per-program emulator at setup
# time (make, as, ld, gcc, -lgmp -lgmpxx -lstdc++) from the sources shipped in
# the tarball's zisk/ directory. A worker started with --emulator never uses
# any of this; keeping it in the image makes the emulator choice a runtime
# flag instead of an image variant.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential libgmp-dev \
    && rm -rf /var/lib/apt/lists/*
COPY --from=toolchain --chown=root:root --chmod=0755 \
     /opt/zisk-dist/bin/zisk-worker-gpu /opt/zisk/bin/zisk-worker
COPY --from=toolchain --chown=root:root --chmod=0755 \
     /opt/zisk-dist/bin/cargo-zisk-dev-gpu /opt/zisk/bin/cargo-zisk-dev
COPY --from=toolchain --chown=root:root \
     /opt/zisk-dist/bin/libziskc.a /opt/zisk-dist/bin/libziskclib.a /opt/zisk/bin/
COPY --from=toolchain --chown=zisk:zisk /opt/zisk-dist/zisk /opt/zisk/zisk
COPY --chown=root:root --chmod=0755 docker/zisk-stack/prepare-keys.sh /usr/local/bin/zisk-prepare-keys
COPY --chown=root:root docker/zisk-stack/keys.sha256 /opt/zisk/keys.sha256
COPY --chown=root:root docker/zisk-stack/worker.toml /etc/zisk/worker.toml
# The key volume mounts here: provingKey/ and provingKeySnark/ inside it.
RUN mkdir -p /opt/zisk/keys && chown zisk:zisk /opt/zisk/keys
# Ask the NVIDIA container toolkit for the GPU and the driver libraries.
ENV NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility
LABEL org.opencontainers.image.title="zksync-os-zisk-worker" \
      org.opencontainers.image.description="ZiSK GPU worker for the ZKsync OS second proof system; holds the proving keys resident"
USER zisk
WORKDIR /var/cache/zisk
ENTRYPOINT ["zisk-worker"]
CMD ["--config", "/etc/zisk/worker.toml"]

# ---------------------------------------------------------------------------
# Optional helper: build the daemon from this checkout inside a container and
# export the binary into out/, for developer builds and the CI dev images. A
# release build skips this and puts the released binary in out/ instead.
FROM rust:1-bookworm AS prover-build
WORKDIR /src
# The prover's only path dependency is the aggregator crate (host build).
COPY guest-aggregator/ guest-aggregator/
COPY prover/ prover/
RUN cargo build --release --locked --manifest-path prover/Cargo.toml \
        --bin zksync-os-zisk-prover-service

FROM scratch AS prover-export
COPY --from=prover-build /src/prover/target/release/zksync-os-zisk-prover-service /zksync-os-zisk-prover-service

# ---------------------------------------------------------------------------
FROM runtime-base AS prover
# The CPU cargo-zisk: only its `remote` subcommands run here.
COPY --from=toolchain --chown=root:root --chmod=0755 \
     /opt/zisk-dist/bin/cargo-zisk-cpu /opt/zisk/bin/cargo-zisk
# Guest ELFs from the reproducible builds, re-verified against the recorded
# hashes so a stale out/ can never ship.
COPY out/zksync-os-zisk-guest out/zksync-os-zisk-guest-aggregator /app/elf/
COPY guest/GUEST_ELF_SHA256 /app/elf/zksync-os-zisk-guest.sha256
COPY guest-aggregator/GUEST_ELF_SHA256 /app/elf/zksync-os-zisk-guest-aggregator.sha256
RUN cd /app/elf \
    && echo "$(cut -d' ' -f1 < zksync-os-zisk-guest.sha256)  zksync-os-zisk-guest" | sha256sum -c - \
    && echo "$(cut -d' ' -f1 < zksync-os-zisk-guest-aggregator.sha256)  zksync-os-zisk-guest-aggregator" | sha256sum -c -
COPY --chown=root:root --chmod=0755 out/zksync-os-zisk-prover-service /usr/local/bin/zksync-os-zisk-prover-service
RUN mkdir -p /var/lib/zisk-prover && chown zisk:zisk /var/lib/zisk-prover
EXPOSE 3313
LABEL org.opencontainers.image.title="zksync-os-zisk-prover" \
      org.opencontainers.image.description="ZKsync OS ZiSK prover daemon: polls the sequencer and proves through a resident ZiSK coordinator"
USER zisk
WORKDIR /var/lib/zisk-prover
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3313/metrics > /dev/null || exit 1
ENTRYPOINT ["zksync-os-zisk-prover-service"]
