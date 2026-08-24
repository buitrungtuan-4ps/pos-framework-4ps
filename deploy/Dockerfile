# syntax=docker/dockerfile:1
#
# pos_cloud image (ADR-0044). Multi-stage: a Rust builder produces the release binary,
# and a slim runtime carries only that binary plus CA roots. The build context is the
# repository root (see deploy/compose.yml `build.context: ..`); .dockerignore keeps it
# to the source cargo needs.

# ---- builder --------------------------------------------------------------------------
# The full rust image (not -slim) carries gcc + make, which `ring` — the pinned rustls
# provider behind the webhook TLS sender and the NATS client (ADR-0031, ADR-0038) —
# compiles a little C and assembly with. No OpenSSL is installed because the whole TLS
# stack is rustls; `aws-lc-rs` is kept out by tokio-rustls's feature pinning, so nothing
# here needs a system crypto library. The tag matches rust-toolchain.toml exactly.
FROM rust:1.94.1-bookworm AS builder
WORKDIR /build
COPY . .
# `--locked` builds exactly the versions in Cargo.lock, so a build today and a build at
# fork time produce the same bits. Only pos_cloud and its dependencies compile; the rest
# of the workspace is never built. Strip debug info to keep the shipped binary small.
RUN cargo build --release --locked -p pos-cloud \
 && strip target/release/pos-cloud

# ---- runtime --------------------------------------------------------------------------
# Only the binary and CA roots, per ADR-0044. The CA roots let any future outbound TLS
# (off-box rclone backups, an operator's NATS-over-TLS) verify; the webhook sender already
# carries its own compiled-in Mozilla root set (webpki-roots), so it needs nothing here.
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --user-group --no-create-home poscloud
COPY --from=builder /build/target/release/pos-cloud /usr/local/bin/pos-cloud
# Runs unprivileged as a fixed uid. deploy/compose.yml mounts the config read-only; the
# uid is a contract with bootstrap.sh (P8b), which chowns the mounted cloud.toml to 10001
# so a 600 secret file stays readable by the app user (root ignores the mode; this uid
# does not). The process reaches no state it does not own.
USER poscloud
# Plain HTTP inside the container; Caddy terminates real TLS in front (ADR-0044).
EXPOSE 8080
ENV RUST_LOG=info
# pos_cloud reads its config path from POS_CLOUD_CONFIG (set by compose) — main.rs.
ENTRYPOINT ["/usr/local/bin/pos-cloud"]
