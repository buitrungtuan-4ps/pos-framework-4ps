# syntax=docker/dockerfile:1
#
# Caddy with the Cloudflare DNS provider (ADR-0044). TLS is issued over DNS-01 so the box
# needs no inbound :80 reachable to ACME and the DNS record can stay grey-clouded. The
# stock caddy image ships no DNS providers, so xcaddy builds one in. The builder and the
# runtime share one immutable Caddy version; digest-lock both at fork.
#
# This image is ONLY used on the Cloudflare DNS-01 path (a managed DOMAIN). The sslip.io
# fallback issues over HTTP-01 with the stock official image and never builds this — see
# the deploy workflow's per-DOMAIN branch and deploy/Caddyfile.
#
# Version: 2.8.4 could not be built with a current caddy-dns/cloudflare — xcaddy resolved
# go.uber.org/zap past the point where the experimental `zapslog.HandlerOptions` that
# caddy 2.8.4 still references was removed, so `go build` failed with `undefined:
# zapslog.HandlerOptions`. Building a matched-modern Caddy (2.10.0) with the plugin resolves
# a self-consistent module graph. Keep the builder and runtime on the same version.
FROM caddy:2.10.0-builder AS builder
RUN xcaddy build --with github.com/caddy-dns/cloudflare

FROM caddy:2.10.0
COPY --from=builder /usr/bin/caddy /usr/bin/caddy
