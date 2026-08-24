# syntax=docker/dockerfile:1
#
# pos_cloud image (ADR-0044). Cross-compiled, never emulated: the Rust toolchain runs on the
# CI runner's native architecture ($BUILDPLATFORM) and emits a binary for the box's architecture
# ($TARGETPLATFORM — amd64 or arm64), so an arm64 image (Oracle Ampere, AWS Graviton) builds at
# native speed instead of an hour under QEMU. The runtime stage runs no commands at all, so no
# stage of this image is ever emulated at build time. Build context is the repo root
# (deploy/compose.yml `build.context: ..`); .dockerignore keeps it to the source cargo needs.

# ---- dashboard (runs natively on the runner) ------------------------------------------
# Builds the SolidJS back-office dashboard (ADR-0060) to dashboard/dist, which the Rust builder
# embeds with rust-embed. Pinned to $BUILDPLATFORM so it runs on the runner's own CPU (a pure
# JS/TS build is architecture-independent — the same dist embeds into an amd64 or arm64 binary).
FROM --platform=$BUILDPLATFORM node:22-bookworm AS dashboard
WORKDIR /dashboard
# Lockfile first for layer caching; `--frozen-lockfile` fails if pnpm-lock.yaml is out of date.
COPY dashboard/package.json dashboard/pnpm-lock.yaml ./
RUN npm install --global pnpm@10 && pnpm install --frozen-lockfile
COPY dashboard/ ./
# `pnpm build` = tsc --noEmit && the no-hardcoded-strings lint && vite build.
RUN pnpm build

# ---- builder (runs natively on the runner) --------------------------------------------
# Pinned to $BUILDPLATFORM so cargo compiles on the runner's own CPU. The full rust image (not
# -slim) carries gcc + make that `ring` — the pinned rustls provider behind the webhook TLS
# sender and the NATS client (ADR-0031, ADR-0038) — compiles a little C and assembly with; the
# aarch64 cross toolchain adds the linker plus the arm64 libc and headers so `ring` cross-builds.
# No OpenSSL is installed because the whole TLS stack is rustls; `aws-lc-rs` is kept out by
# tokio-rustls's feature pinning, so nothing here needs a system crypto library. The tag matches
# rust-toolchain.toml exactly.
FROM --platform=$BUILDPLATFORM rust:1.94.1-bookworm AS builder
ARG TARGETARCH
# The GNU cross toolchain for arm64: the compiler/linker (gcc-aarch64-linux-gnu) AND the arm64
# target libc dev headers (libc6-dev-arm64-cross). The headers are a *Recommends* of the gcc
# package, so `--no-install-recommends` drops them unless named explicitly — and without them
# `ring`'s C (curve25519.c etc.) fails with "fatal error: … No such file or directory". Both are
# only exercised on an arm64 target; on an amd64 target the native gcc already in the image builds.
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
 && rm -rf /var/lib/apt/lists/*
# Both targets present, so either box architecture builds from this one image.
RUN rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
# `ring`'s build script reads CC_<triple>; cargo reads CARGO_TARGET_<TRIPLE>_LINKER. Point both at
# the cross gcc for the arm64 build. The amd64 build needs neither (host gcc, host linker).
ENV CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
WORKDIR /build
COPY . .
# The real dashboard build embeds via rust-embed (ADR-0060). dashboard/dist is excluded from the
# build context (.dockerignore) and built fresh in the node stage above, so copy it in before the
# compile — otherwise build.rs's placeholder page would be what ships.
COPY --from=dashboard /dashboard/dist ./dashboard/dist
# Map Docker's TARGETARCH (amd64/arm64) to the Rust triple, build ONLY pos-cloud and its deps
# (`--locked` => exactly the versions in Cargo.lock, so a build today and a build at fork time
# produce the same bits), and stage the binary at a fixed path for the runtime COPY. The
# `strip = "symbols"` in the workspace [profile.release] keeps it small with no arch-specific strip.
RUN case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu ;; \
      arm64) triple=aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported TARGETARCH: '$TARGETARCH'" >&2; exit 1 ;; \
    esac \
 && cargo build --release --locked --target "$triple" -p pos-cloud \
 && cp "target/$triple/release/pos-cloud" /pos-cloud

# ---- runtime ($TARGETPLATFORM, never executed at build time) --------------------------
# Only the binary and CA roots, per ADR-0044. This stage runs NO commands: the CA bundle is
# copied from the builder (architecture-independent PEM text) and the process runs as a fixed
# numeric uid, so the target-arch image is assembled without ever executing target-arch code —
# which is what removes QEMU from the build entirely.
FROM debian:bookworm-slim AS runtime
# The trust store any future outbound TLS verifies against (off-box rclone backups, an operator's
# NATS-over-TLS); the webhook sender already carries its own compiled-in Mozilla roots
# (webpki-roots), so it needs nothing here. `rustls-native-certs` (via openssl-probe) reads this
# exact path on Debian. Copied rather than apt-installed to keep this stage command-free.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /pos-cloud /usr/local/bin/pos-cloud
# Runs unprivileged as a fixed uid. deploy/compose.yml mounts the config read-only; the uid is a
# contract with bootstrap.sh (P8b), which chowns the mounted cloud.toml to 10001 so a 600 secret
# file stays readable by the app user (root ignores the mode; this uid does not). Numeric so no
# /etc/passwd entry — and no `useradd` command — is needed. The process reaches no state it does
# not own.
USER 10001:10001
# Plain HTTP inside the container; Caddy terminates real TLS in front (ADR-0044).
EXPOSE 8080
ENV RUST_LOG=info
# pos_cloud reads its config path from POS_CLOUD_CONFIG (set by compose) — main.rs.
ENTRYPOINT ["/usr/local/bin/pos-cloud"]
