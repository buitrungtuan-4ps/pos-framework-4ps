# syntax=docker/dockerfile:1
#
# Caddy with the Cloudflare DNS provider (ADR-0044). TLS is issued over DNS-01 so the box
# needs no inbound :80 reachable to ACME and the DNS record can stay grey-clouded. The
# stock caddy image ships no DNS providers, so xcaddy builds one in. The builder and the
# runtime share one immutable Caddy version; digest-lock both at fork.
FROM caddy:2.8.4-builder AS builder
RUN xcaddy build --with github.com/caddy-dns/cloudflare

FROM caddy:2.8.4
COPY --from=builder /usr/bin/caddy /usr/bin/caddy
