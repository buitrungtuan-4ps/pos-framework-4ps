# syntax=docker/dockerfile:1
#
# The pos_edge container image (ADR-0113, roadmap-v3 Program C): one store's edge, for a hosted
# placement (ADR-0110). It wraps the **released** `pos_edge` binary rather than compiling one, so
# the container runs exactly the bytes a store PC runs, stamped and signed by the release, and an
# update is a new image over the same volume, never the edge installing itself: a container has no
# `bin/current` layout, so the edge's own updater logs that it found none and starts nothing.
#
# Build context is this directory with the binary beside the Dockerfile:
#
#   cp target/x86_64-unknown-linux-gnu/release/pos-edge deploy/edge-image/pos-edge
#   docker build -t pos-edge:local deploy/edge-image
#
# The runtime stage runs no commands, as deploy/Dockerfile's does, so an arm64 image is assembled
# on an amd64 runner without emulation when the arm64 binary is the one copied in.
# Debian 13: its glibc is newer than the Ubuntu 24.04 runner's that links the release binary. No CA
# bundle is added: the edge's client to its cloud carries its own compiled-in Mozilla roots (ADR-0054).
FROM debian:trixie-slim

COPY pos-edge /usr/local/bin/pos-edge

# One store, one uid (ADR-0113 "one container, one volume, one user"). The host agent passes a
# distinct uid per store with `--user`; this default keeps a hand-run container unprivileged.
USER 10002:10002

# The store's whole state: config.toml, the SQLite database, its `.pre-update` copy and the vault.
# One volume per store; nothing else is mounted, and the container gets no handle on its runtime.
VOLUME ["/var/lib/pos-edge"]
WORKDIR /var/lib/pos-edge
ENV POS_EDGE_CONFIG=/var/lib/pos-edge/config.toml \
    RUST_LOG=info

# Plain HTTP inside; the host's reverse proxy terminates TLS for the store's hostname, and the
# port is published on 127.0.0.1 only (ADR-0113).
EXPOSE 8787

# The exit code is the outcome (ADR-0113): 0 stopped, 10 restart wanted, 11 stopped with the
# outbox undrained. Whoever started the container reads it; nothing inside acts on it.
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/pos-edge"]
