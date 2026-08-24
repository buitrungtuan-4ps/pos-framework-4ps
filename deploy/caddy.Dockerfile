# syntax=docker/dockerfile:1
#
# Caddy with the Cloudflare DNS provider (ADR-0044). Cross-compiled like the app image: the Go
# toolchain runs on the runner's native architecture ($BUILDPLATFORM) and emits a binary for the
# box's architecture (GOARCH from $TARGETARCH), so the arm64 image builds at native speed with no
# QEMU. TLS is issued over DNS-01 so the box needs no inbound :80 reachable to ACME and the DNS
# record can stay grey-clouded. The stock caddy image ships no DNS providers, so xcaddy builds one
# in. The builder and the runtime share one immutable Caddy version; digest-lock both at fork.
#
# This image is ONLY used on the Cloudflare DNS-01 path (a managed DOMAIN). The sslip.io fallback
# issues over HTTP-01 with the stock official image and never builds this — see the deploy
# workflow's per-DOMAIN branch and deploy/Caddyfile.
#
# Version: 2.8.4 could not be built with a current caddy-dns/cloudflare — xcaddy resolved
# go.uber.org/zap past the point where the experimental `zapslog.HandlerOptions` that caddy 2.8.4
# still references was removed, so `go build` failed with `undefined: zapslog.HandlerOptions`.
# Building a matched-modern Caddy (2.10.0) with the plugin resolves a self-consistent module graph.
# Keep the builder and runtime on the same version.
FROM --platform=$BUILDPLATFORM caddy:2.10.0-builder AS builder
ARG TARGETARCH
# xcaddy honours GOOS/GOARCH, so a native Go cross-build produces the box's binary. Docker's
# TARGETARCH (amd64/arm64) is exactly Go's GOARCH spelling, so it maps straight through. Output
# path is the caddy builder image's default, /usr/bin/caddy.
ENV GOOS=linux GOARCH=$TARGETARCH
RUN xcaddy build --with github.com/caddy-dns/cloudflare

FROM caddy:2.10.0
COPY --from=builder /usr/bin/caddy /usr/bin/caddy
