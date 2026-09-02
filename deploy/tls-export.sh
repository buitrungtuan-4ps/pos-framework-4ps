#!/usr/bin/env bash
# deploy/tls-export.sh — republish Caddy's ACME certificate into ./secrets/tls (ADR-0090).
#
# The two ACME modes leave the only real certificate on the box inside Caddy's own `caddy_data`
# volume, at a path whose directory name is the ACME directory URL — private layout belonging to
# another service. Other consumers (the event bus of ADR-0089 first) need that certificate at a
# stable path, and they need it to follow renewals: Caddy renews roughly 30 days before expiry, so a
# consumer that never re-reads keeps serving the old certificate until it expires and then fails,
# silently, weeks later.
#
# This script is the one place that reach happens, with a renewal hook and an exit status:
#
#   * it reads the certificate THROUGH the container, so it needs neither root nor any knowledge of
#     where Docker keeps volume data;
#   * it globs for the ACME-directory segment and REFUSES when the glob matches anything other than
#     exactly one file, rather than picking one;
#   * it writes only when the bytes changed, so a consumer is only signalled on a real renewal;
#   * it signals the services named in TLS_RELOAD_SERVICES (in secrets/caddy.env, space-separated,
#     empty by default) with SIGHUP after a change.
#
# Exit status: 0 exported or already current · non-zero nothing was exported, reason on stderr.
# Run it from cron; the line is in docs/deploy-runbook.md. Nothing yet alerts on a stale export —
# that follow-up is flagged in ADR-0090.
set -euo pipefail
umask 077

HERE="$(cd "$(dirname "$0")" && pwd)"
SECRETS="$HERE/secrets"
COMPOSE="$HERE/compose.yml"
TLS_DIR="$SECRETS/tls"

fail() {
  echo "tls-export: $*" >&2
  exit 1
}

[ -e "$SECRETS/caddy.env" ] || fail "no $SECRETS/caddy.env — run bootstrap.sh first"
MODE="$(sed -n 's/^TLS_MODE=//p' "$SECRETS/caddy.env")"
DOMAIN="$(sed -n 's/^DOMAIN=//p' "$SECRETS/caddy.env")"
RELOAD="$(sed -n 's/^TLS_RELOAD_SERVICES=//p' "$SECRETS/caddy.env")"
[ -n "$DOMAIN" ] || fail "no DOMAIN in $SECRETS/caddy.env"

case "${MODE:-acme-http01}" in
  acme-http01 | acme-dns01) ;;
  byo-cert)
    echo "tls-export: TLS_MODE=byo-cert — the certificate in $TLS_DIR is the operator's; nothing to export"
    exit 0
    ;;
  external)
    echo "tls-export: TLS_MODE=external — no certificate is issued on this box; nothing to export"
    exit 0
    ;;
  *) fail "unknown TLS_MODE='$MODE' in $SECRETS/caddy.env" ;;
esac

command -v docker >/dev/null 2>&1 || fail "docker not found"

# Resolve the certificate inside the container. The path segment between `certificates/` and the
# hostname is the ACME directory URL, which is Caddy's private layout and must not be hard-coded —
# so glob, and refuse to guess when the answer is not unique (two ACME directories, a staging
# certificate left behind, a hostname change mid-issuance).
listing="$(docker compose -f "$COMPOSE" exec -T caddy sh -c \
  "ls -1 /data/caddy/certificates/*/$DOMAIN/$DOMAIN.crt 2>/dev/null" 2>/dev/null || true)"
listing="$(printf '%s\n' "$listing" | tr -d '\r' | grep . || true)"
found="$(printf '%s' "$listing" | grep -c . || true)"
case "$found" in
  1) ;;
  0) fail "no certificate for $DOMAIN inside the caddy container yet (is it running, and has ACME issued?)" ;;
  *) fail "found $found certificates for $DOMAIN inside the caddy container; refusing to guess which one is live:
$listing" ;;
esac
crt_path="$listing"
key_path="${crt_path%.crt}.key"

mkdir -p "$TLS_DIR"
chmod 700 "$TLS_DIR"
tmp_crt="$(mktemp "$TLS_DIR/.fullchain.XXXXXX")"
tmp_key="$(mktemp "$TLS_DIR/.privkey.XXXXXX")"
trap 'rm -f "$tmp_crt" "$tmp_key"' EXIT

docker compose -f "$COMPOSE" exec -T caddy cat "$crt_path" > "$tmp_crt" \
  || fail "could not read $crt_path from the caddy container"
docker compose -f "$COMPOSE" exec -T caddy cat "$key_path" > "$tmp_key" \
  || fail "could not read $key_path from the caddy container"
[ -s "$tmp_crt" ] || fail "$crt_path read back empty"
[ -s "$tmp_key" ] || fail "$key_path read back empty"

# Compare bytes, not timestamps: Caddy rewrites its own files on every renewal check, and a
# consumer must only be signalled when the certificate actually changed.
if cmp -s "$tmp_crt" "$TLS_DIR/fullchain.pem" && cmp -s "$tmp_key" "$TLS_DIR/privkey.pem"; then
  echo "tls-export: $DOMAIN already current in $TLS_DIR"
  exit 0
fi

chmod 644 "$tmp_crt"
chmod 600 "$tmp_key"
mv -f "$tmp_crt" "$TLS_DIR/fullchain.pem"
mv -f "$tmp_key" "$TLS_DIR/privkey.pem"
trap - EXIT
date -u '+%Y-%m-%dT%H:%M:%SZ' > "$TLS_DIR/exported-at"
echo "tls-export: exported $DOMAIN to $TLS_DIR (from $crt_path)"

# Signal the consumers, if any are configured. Empty until a slice adds one — ADR-0089's bus is the
# first, and it sets TLS_RELOAD_SERVICES=nats.
for service in $RELOAD; do
  if docker compose -f "$COMPOSE" kill -s HUP "$service" >/dev/null 2>&1; then
    echo "tls-export: SIGHUP -> $service"
  else
    echo "tls-export: could not signal '$service' (not running?)" >&2
  fi
done
