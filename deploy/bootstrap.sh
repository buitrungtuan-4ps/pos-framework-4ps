#!/usr/bin/env bash
# deploy/bootstrap.sh — idempotent, server-side bootstrap for one country cell (P8b, ADR-0044).
#
# Run once on the VPS (the deploy workflow runs it over SSH, P8c). It mints the *internal*
# operational secrets ON THE BOX and writes them into ./secrets/, which is git-ignored
# (/deploy/secrets/ in .gitignore) and never returned to GitHub. It then brings the Compose
# stack up. Re-running is safe: an existing secret is kept, never rotated — so a second run
# does not lock you out of a database whose password you already deployed.
#
# What it generates (all mode 600, under ./secrets/):
#   pos.env         POSTGRES_USER / POSTGRES_PASSWORD / POSTGRES_DB
#   cloud.toml      pos_cloud config (bind, database_url with the postgres password);
#                   chowned to uid 10001 so the non-root app container can read it
#   nats.conf       NATS JetStream + a token, plus TLS on the client port once a certificate
#                   exists (ADR-0089). Rewritten each run with the existing token carried across —
#                   the TLS half is derived, and a run that cannot read the token refuses.
#   garage.toml     Garage single-node config with an rpc_secret. The S3 access keys the cloud
#                   reads artifacts with are minted from the running server (step 7b) and
#                   appended to cloud.toml as [artifacts]; Garage generates them, so they are
#                   the one secret here that is captured rather than pre-generated.
#   caddy.env       TLS_MODE / DOMAIN / ACME_EMAIL / CF_DNS_API_TOKEN — the only values that
#                   come from OUTSIDE the box (GitHub secrets in the deploy workflow), plus
#                   TLS_RELOAD_SERVICES. Unlike the generated secrets above, these are SUPPLIED
#                   configuration: when the environment provides DOMAIN the file is rewritten,
#                   so changing a secret and redeploying actually takes effect.
#   Caddyfile       the per-mode Caddyfile for the selected TLS_MODE, copied from
#                   ./Caddyfile.d/<mode>.caddy (ADR-0090). Generated, never committed — nothing
#                   here overwrites a version-controlled file.
#   tls/            fullchain.pem + privkey.pem, the one certificate path every consumer reads.
#                   Populated by the operator on TLS_MODE=byo-cert, by ./tls-export.sh on the
#                   two ACME modes, and by nobody on TLS_MODE=external.
#   setup-token.txt a one-time super-admin setup token, printed once — it authorizes
#                   enrolling the first super-admin (password + TOTP) through the
#                   first-boot provisioning route (wired in P8c), and is void thereafter
#
# It also writes ./.env beside compose.yml — NOT a secret, just the port publishes and image
# tags the selected posture implies, so a later `docker compose up -d` typed by hand reproduces
# the same posture instead of silently reverting to the defaults (ADR-0090).
#
# TLS_MODE (ADR-0090) is one of:
#   acme-http01  Caddy issues over HTTP-01 / TLS-ALPN. The default; needs :80 reachable.
#   acme-dns01   Caddy issues over Cloudflare DNS-01. Needs CF_DNS_API_TOKEN and the plugin image.
#   byo-cert     the operator installed secrets/tls/{fullchain,privkey}.pem. No ACME.
#   external     TLS terminates upstream; this box publishes HTTP only and trusts two proxy hops.
# It is never inferred from DOMAIN: inference cannot express the last two postures at all, and its
# fallthrough silently downgraded a managed domain with an empty token to a method that cannot work
# on a DNS-only record.
#
# First run needs the reach-the-box + certificate values in the environment, e.g.:
#   DOMAIN=cloud.example.com TLS_MODE=acme-dns01 ACME_EMAIL=ops@example.com \
#     CF_DNS_API_TOKEN=xxxx ./bootstrap.sh
# With no purchased domain, use the sslip.io fallback (the default mode, no Cloudflare token):
#   DOMAIN=203-0-113-9.sslip.io ACME_EMAIL=ops@example.com ./bootstrap.sh
#
# Set POS_BOOTSTRAP_NO_UP=1 to generate secrets without starting the stack.
set -euo pipefail
umask 077

HERE="$(cd "$(dirname "$0")" && pwd)"
SECRETS="$HERE/secrets"
COMPOSE="$HERE/compose.yml"
APP_UID=10001   # MUST match the poscloud uid created in deploy/Dockerfile.

# 32 (or $1) random bytes as lowercase hex. openssl if present, else /dev/urandom.
rand_hex() {
  local n="${1:-32}"
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex "$n"
  else
    od -An -tx1 -N "$n" /dev/urandom | tr -d ' \n'
  fi
}

mkdir -p "$SECRETS"
chmod 700 "$SECRETS"

# 1. PostgreSQL credentials.
if [ ! -e "$SECRETS/pos.env" ]; then
  cat > "$SECRETS/pos.env" <<EOF
POSTGRES_USER=pos
POSTGRES_PASSWORD=$(rand_hex 24)
POSTGRES_DB=poscloud
EOF
  chmod 600 "$SECRETS/pos.env"
  echo "create pos.env"
else
  echo "keep   pos.env"
fi
# Read the credentials back so cloud.toml matches whether or not pos.env was just created.
PG_USER="$(sed -n 's/^POSTGRES_USER=//p' "$SECRETS/pos.env")"
PG_PW="$(sed -n 's/^POSTGRES_PASSWORD=//p' "$SECRETS/pos.env")"
PG_DB="$(sed -n 's/^POSTGRES_DB=//p' "$SECRETS/pos.env")"

# 1b. The TLS posture (ADR-0090). Resolved BEFORE cloud.toml, because the posture decides
#     trusted_proxy_hops and that value has to be right the first time cloud.toml is written.
#
#     caddy.env holds SUPPLIED configuration, not generated secrets, so the "keep, never rotate"
#     rule does not apply to it: when the environment provides DOMAIN the file is rewritten. That is
#     deliberate and it fixes a real wart — until now, changing the DOMAIN secret and redeploying
#     did nothing at all, because the file was kept. An empty DOMAIN counts as "not supplied" (the
#     deploy workflow interpolates an empty string for an unset secret), so a re-run with no
#     environment keeps whatever the box already has.
tls_mode_is_valid() {
  case "$1" in
    acme-http01 | acme-dns01 | byo-cert | external) return 0 ;;
    *) return 1 ;;
  esac
}

if [ -n "${DOMAIN:-}" ]; then
  TLS_MODE="${TLS_MODE:-acme-http01}"
  tls_mode_is_valid "$TLS_MODE" ||
    { echo "TLS_MODE='$TLS_MODE' is not one of: acme-http01 acme-dns01 byo-cert external" >&2; exit 1; }
  # Per-mode inputs, checked here and REFUSED rather than downgraded. The old code fell through to
  # HTTP-01 when a managed domain had no token, which cannot work on a DNS-only record: the cell got
  # no certificate and the log said HTTP-01 was chosen on purpose.
  case "$TLS_MODE" in
    acme-http01 | acme-dns01)
      : "${ACME_EMAIL:?set ACME_EMAIL=you@example.com — TLS_MODE=$TLS_MODE issues over ACME}"
      ;;
    *) : "${ACME_EMAIL:=}" ;;
  esac
  if [ "$TLS_MODE" = "acme-dns01" ]; then
    : "${CF_DNS_API_TOKEN:?TLS_MODE=acme-dns01 needs CF_DNS_API_TOKEN (Cloudflare Zone:DNS:Edit on the one zone). It is not optional and is never downgraded to HTTP-01, which cannot answer a challenge for a DNS-only record}"
  else
    : "${CF_DNS_API_TOKEN:=}"
  fi
  cat > "$SECRETS/caddy.env" <<EOF
# Caddy TLS posture and hostname (written by bootstrap.sh from the deploy environment; ADR-0090).
# Never commit. TLS_MODE is one of acme-http01 | acme-dns01 | byo-cert | external.
TLS_MODE=$TLS_MODE
DOMAIN=$DOMAIN
ACME_EMAIL=$ACME_EMAIL
CF_DNS_API_TOKEN=$CF_DNS_API_TOKEN
# Compose services that tls-export.sh should SIGHUP after a certificate change (space-separated).
# The event bus reads the exported certificate (ADR-0089) and re-reads it on SIGHUP, which is what
# keeps it from serving a stale certificate until expiry and then going dark silently.
TLS_RELOAD_SERVICES=${TLS_RELOAD_SERVICES:-nats}
EOF
  chmod 600 "$SECRETS/caddy.env"
  echo "write  caddy.env (TLS_MODE=$TLS_MODE, DOMAIN=$DOMAIN)"
elif [ ! -e "$SECRETS/caddy.env" ]; then
  echo "set DOMAIN=your.host (or <vps-ip>.sslip.io) before the first bootstrap" >&2
  exit 1
else
  echo "keep   caddy.env (no DOMAIN in the environment)"
fi

CADDY_TLS_MODE="$(sed -n 's/^TLS_MODE=//p' "$SECRETS/caddy.env")"
CADDY_DOMAIN="$(sed -n 's/^DOMAIN=//p' "$SECRETS/caddy.env")"
# A caddy.env written before ADR-0090 has no TLS_MODE line. Do NOT default it to acme-http01 —
# a box that was on the Cloudflare path would silently change posture and stop renewing. Infer the
# posture once from the rule that file was written under, record it, and say so.
if [ -z "$CADDY_TLS_MODE" ]; then
  CADDY_CF_TOKEN="$(sed -n 's/^CF_DNS_API_TOKEN=//p' "$SECRETS/caddy.env")"
  case "$CADDY_DOMAIN" in
    *.sslip.io) CADDY_TLS_MODE=acme-http01 ;;
    *) [ -n "$CADDY_CF_TOKEN" ] && CADDY_TLS_MODE=acme-dns01 || CADDY_TLS_MODE=acme-http01 ;;
  esac
  printf 'TLS_MODE=%s\n' "$CADDY_TLS_MODE" >> "$SECRETS/caddy.env"
  echo "migrate caddy.env had no TLS_MODE; recorded the posture it was already running: $CADDY_TLS_MODE"
fi
tls_mode_is_valid "$CADDY_TLS_MODE" ||
  { echo "TLS_MODE='$CADDY_TLS_MODE' in $SECRETS/caddy.env is not a known posture" >&2; exit 1; }

# A caddy.env written before the bus consumed the certificate has no reload list. Add it, so a
# renewal actually reaches the broker instead of leaving it on a certificate that will expire.
if ! grep -q '^TLS_RELOAD_SERVICES=' "$SECRETS/caddy.env"; then
  printf 'TLS_RELOAD_SERVICES=nats\n' >> "$SECRETS/caddy.env"
  echo "migrate caddy.env had no TLS_RELOAD_SERVICES; set it to 'nats' so a renewal reloads the bus"
fi

# How many proxies in front of pos_cloud are trusted to have appended to X-Forwarded-For. One is
# the bundled Caddy. Under `external` the chain is client, upstream-terminator, caddy — so the real
# client is two back, and leaving this at 1 would key the /admin/login rate limit on the
# terminator's single address: every admin in one bucket, one person's wrong passwords locking out
# the rest (ADR-0067 slice 5, ADR-0090).
case "$CADDY_TLS_MODE" in
  external) TRUSTED_PROXY_HOPS=2 ;;
  *) TRUSTED_PROXY_HOPS=1 ;;
esac

# Refuse a byo-cert posture with no certificate HERE, before anything else is generated. Checking it
# later would mean a first run that mints the one-time super-admin setup token, prints it, and then
# aborts — the token survives in cloud.toml but the operator never sees it again, because a re-run
# takes the "keep" branch and prints nothing.
if [ "$CADDY_TLS_MODE" = "byo-cert" ]; then
  for f in fullchain.pem privkey.pem; do
    [ -s "$SECRETS/tls/$f" ] ||
      { echo "TLS_MODE=byo-cert needs $SECRETS/tls/$f (non-empty). Install the certificate and key there, then re-run." >&2; exit 1; }
  done
fi

# 2. pos_cloud config. The database password lives here (libpq keyword form), never in the
#    environment. The ingest cursor and the retention cron start off — each is armed by
#    editing this file, both deliberate decisions rather than defaults (ADR-0031, ADR-0035).
if [ ! -e "$SECRETS/cloud.toml" ]; then
  # Capture the token now so we can print it in step 6 without re-reading the file — after the
  # chown below the deploy user may no longer be able to read a 600 file owned by $APP_UID.
  SETUP_TOKEN="$(rand_hex 32)"
  INTERNAL_SECRET="$(rand_hex 32)"
  TABLE_TOKEN_SECRET="$(rand_hex 32)"
  cat > "$SECRETS/cloud.toml" <<EOF
# pos_cloud boot config (generated by bootstrap.sh; ADR-0044/ADR-0045). Never commit.
bind = "0.0.0.0:8080"
database_url = "host=postgres port=5432 user=$PG_USER password=$PG_PW dbname=$PG_DB"

# One-time super-admin setup token (ADR-0045): it authorises enrolling the first super-admin
# at POST /admin/setup and is void once that admin exists. Remove this line after enrolment to
# turn the setup route off (a 404).
admin_setup_token = "$SETUP_TOKEN"

# The shared secret the three /internal/* routes require in X-Pos-Internal-Key (ADR-0097).
# pos_cloud REFUSES TO START without it: this file is not checked for unknown keys, so if
# absence meant "authentication off" a misspelled key name would look set and be missing.
# It is defence in depth, not a replacement for the proxy deny below — keep both.
internal_shared_secret = "$INTERNAL_SECRET"

# The secret the cloud signs a table's QR token with (ADR-0057). Generated here rather than left
# out, because `pos_cloud` treats absence as "QR ordering off" and logs one warn line on boot —
# so a box provisioned without it prints table QR sheets that lead nowhere, and the only clue is a
# line nobody reads. Turning the feature on should be a decision about the shop, not an accident
# of which secrets the installer happened to write.
table_token_secret = "$TABLE_TOKEN_SECRET"

# How many proxies in front of this process are trusted to have appended to X-Forwarded-For
# (ADR-0090). Derived from TLS_MODE, not hand-set: 1 behind the bundled Caddy, 2 when TLS
# terminates upstream. bootstrap.sh reconciles this line on every run, because getting it wrong
# keys the /admin/login rate limit on the wrong address.
trusted_proxy_hops = $TRUSTED_PROXY_HOPS

# The ingest cursor is off until stores publish to JetStream (ADR-0031). To arm it,
# uncomment and use the token from secrets/nats.conf. The token belongs in the URL's userinfo
# exactly as written — link-nats lifts it into the connect options, because async-nats itself reads
# credentials only from there and would otherwise drop it (ADR-0089's correction).
#
# The scheme and host are NOT free choices (production-readiness D2). Once secrets/tls holds a
# certificate this script puts TLS on the broker's client port for EVERY client, this container
# included — so `nats://` is refused as plaintext, and `tls://…@nats:4222` fails hostname
# verification, because the certificate is issued for DOMAIN and `nats` is a Docker network alias
# that can never appear on it. Use the public name the certificate actually carries:
#
#   url = "tls://:THE_NATS_TOKEN@YOUR_DOMAIN:4222"
#
# which means this container reaches its own host by its public address — an ordinary hairpin on a
# VPS with a public IP. Where that cannot hairpin, the other working shape is to leave the broker's
# TLS off (no certificate in secrets/tls, so the port stays on 127.0.0.1) and use the plaintext
# `nats://:THE_NATS_TOKEN@nats:4222`, which is safe only because the port is then closed to the
# internet and reachable from the Docker network alone. Do not mix the two.
#
# `stream` must be the same name every store publishes into, and the console's new-store wizard
# generates POS_FLEET / pos.fleet.events into each store's config.toml (ADR-0087 Amendment 1). One
# stream, one subject, fleet-wide: this cursor binds ONE stream, so a fork that renames either must
# rename it on both sides at once — otherwise the fleet publishes into a stream nobody reads, and
# nothing anywhere reports an error. `filter_subject` is left unset, which means every subject the
# stream captures:
# [nats]
# url = "tls://:THE_NATS_TOKEN@YOUR_DOMAIN:4222"
# stream = "POS_FLEET"
# durable = "cloud_ingest"

# retention_days is a legal decision, never a code default (ADR-0035). Set it from the
# country's configured retention period to arm the PII-masking cron; absent = cron off.
# retention_days = 365
EOF
  chmod 600 "$SECRETS/cloud.toml"
  echo "create cloud.toml"
  CLOUD_TOML_CREATED=1
else
  echo "keep   cloud.toml"
  CLOUD_TOML_CREATED=0
  # trusted_proxy_hops is DERIVED from TLS_MODE, not a secret, so unlike everything else in this
  # file it is reconciled on every run — otherwise switching a live cell to TLS_MODE=external would
  # leave the hop count at 1 and quietly collapse every admin onto one rate-limit bucket. A previous
  # run chowned this file to $APP_UID mode 600, so a non-root deploy user can neither read nor write
  # it; that is why this uses the same `sudo -n` ladder as the chown below, and why failing at both
  # is reported loudly rather than passed over.
  # ADDING a top-level key means inserting it ABOVE the first table header, never appending it to
  # the end of the file (production-readiness D1). Step 4 appends `[artifacts]` when Garage answers,
  # so on any box that has completed a bootstrap the end of cloud.toml is *inside* that table — and
  # a bare `trusted_proxy_hops` written there becomes `artifacts.trusted_proxy_hops`, which the
  # cloud never reads. It would then quietly keep the default hop count while this script reported
  # "set": exactly the failure the comment above says the reconcile exists to prevent.
  hops_note='# Trusted X-Forwarded-For hops, derived from TLS_MODE (ADR-0090).'
  hops_cmd="if grep -q '^trusted_proxy_hops' '$SECRETS/cloud.toml'; then"
  hops_cmd="$hops_cmd sed -i 's/^trusted_proxy_hops = .*/trusted_proxy_hops = $TRUSTED_PROXY_HOPS/' '$SECRETS/cloud.toml';"
  hops_cmd="$hops_cmd elif grep -q '^\\[' '$SECRETS/cloud.toml'; then"
  hops_cmd="$hops_cmd sed -i '0,/^\\[/s//$hops_note\\ntrusted_proxy_hops = $TRUSTED_PROXY_HOPS\\n\\n&/' '$SECRETS/cloud.toml';"
  hops_cmd="$hops_cmd else printf '\\n%s\\ntrusted_proxy_hops = %s\\n' '$hops_note' '$TRUSTED_PROXY_HOPS' >> '$SECRETS/cloud.toml'; fi"
  if sh -c "$hops_cmd" 2>/dev/null; then
    echo "set    cloud.toml trusted_proxy_hops = $TRUSTED_PROXY_HOPS (TLS_MODE=$CADDY_TLS_MODE)"
  elif command -v sudo >/dev/null 2>&1 && sudo -n sh -c "$hops_cmd" 2>/dev/null; then
    echo "set    cloud.toml trusted_proxy_hops = $TRUSTED_PROXY_HOPS (via sudo; TLS_MODE=$CADDY_TLS_MODE)"
  else
    echo "warn   could not set trusted_proxy_hops = $TRUSTED_PROXY_HOPS in $SECRETS/cloud.toml (need root or passwordless sudo)"
    echo "warn   TLS_MODE=$CADDY_TLS_MODE requires it: set it by hand and restart pos_cloud, or the /admin/login rate limit throttles by the proxy's address instead of the client's"
  fi
  # table_token_secret is APPENDED IF ABSENT and never rewritten. Boxes bootstrapped before this
  # line existed have no QR ordering and no error to show for it — absence means "feature off" plus
  # one warn line at boot — so an upgrade run should turn it on. Rewriting an existing one would
  # invalidate every table QR already printed and stuck to a table, which is why this is the one
  # reconcile in this file that refuses to touch a value it finds.
  qr_note='# QR table-token signing secret (ADR-0057), added by a later bootstrap run.'
  qr_secret="$(rand_hex 32)"
  qr_cmd="if grep -q '^table_token_secret' '$SECRETS/cloud.toml'; then :;"
  qr_cmd="$qr_cmd elif grep -q '^\\[' '$SECRETS/cloud.toml'; then"
  qr_cmd="$qr_cmd sed -i '0,/^\\[/s//$qr_note\\ntable_token_secret = \"$qr_secret\"\\n\\n&/' '$SECRETS/cloud.toml'; echo added;"
  qr_cmd="$qr_cmd else printf '\\n%s\\ntable_token_secret = \"%s\"\\n' '$qr_note' '$qr_secret' >> '$SECRETS/cloud.toml'; echo added; fi"
  if qr_added="$(sh -c "$qr_cmd" 2>/dev/null)"; then
    [ -n "$qr_added" ] && echo "set    cloud.toml table_token_secret (QR ordering was off on this box)"
  elif command -v sudo >/dev/null 2>&1 && qr_added="$(sudo -n sh -c "$qr_cmd" 2>/dev/null)"; then
    [ -n "$qr_added" ] && echo "set    cloud.toml table_token_secret via sudo (QR ordering was off on this box)"
  else
    echo "warn   could not check table_token_secret in $SECRETS/cloud.toml (need root or passwordless sudo); QR ordering may still be off"
  fi
fi
# The pos_cloud container runs as uid $APP_UID and must read this 600 file (root ignores the
# mode; the app user does not). Deploying as root chowns directly; deploying as a non-root sudo
# user (the common cloud default, e.g. Oracle's `ubuntu`) falls back to passwordless `sudo -n`.
# Without either, the app cannot read its config — a warning, not a hard failure, so the rest of
# bootstrap still runs and the cause is visible in the log.
if chown "$APP_UID:$APP_UID" "$SECRETS/cloud.toml" 2>/dev/null; then
  :
elif command -v sudo >/dev/null 2>&1 && sudo -n chown "$APP_UID:$APP_UID" "$SECRETS/cloud.toml" 2>/dev/null; then
  echo "chown  cloud.toml -> $APP_UID (via sudo; non-root deploy user)"
else
  echo "warn   could not chown cloud.toml to $APP_UID (need root or passwordless sudo); pos_cloud may be unable to read it"
fi

# 3. NATS: JetStream, a token, and — once a certificate exists — TLS on a published client port
#    (ADR-0089). The monitoring port (8222) stays internal for the healthcheck, no token.
#
#    Unlike every other generated secret, this file is REWRITTEN each run, carrying the existing
#    token across. It has to be: whether the client port is TLS-wrapped follows from the TLS posture
#    and from whether secrets/tls holds a certificate yet, and both can change on a redeploy. The
#    token is the only thing in here that must survive, so it is read back and reused — and if it
#    cannot be read, the run REFUSES rather than minting a new one, because a rotated token silently
#    breaks every store's publish and the cloud's own cursor.
if [ -e "$SECRETS/nats.conf" ]; then
  NATS_TOKEN="$(sed -n 's/^  token: "\(.*\)"$/\1/p' "$SECRETS/nats.conf")"
  [ -n "$NATS_TOKEN" ] ||
    { echo "could not read the existing NATS token from $SECRETS/nats.conf. Refusing to mint a new one: that would break every store's publish and the cloud's ingest cursor. Recover the token (sudo sed -n 's/  token: //p' secrets/nats.conf) or remove the file deliberately to start fresh." >&2; exit 1; }
  NATS_TOKEN_STATE="carry"
else
  NATS_TOKEN="$(rand_hex 32)"
  NATS_TOKEN_STATE="mint"
fi

# ADR-0089's invariant, made mechanical: the client port never opens without TLS. Publishing 4222
# makes the broker internet-facing — no proxy, no Cloudflare, no firewall in front of it by default
# — so its TLS and its token are the only things protecting the fleet's event stream. The certificate
# comes from secrets/tls (ADR-0090), and on the ACME modes tls-export.sh puts it there *after* the
# first bring-up, so a first deploy legitimately has none: leave the port closed and say why.
if [ -s "$SECRETS/tls/fullchain.pem" ] && [ -s "$SECRETS/tls/privkey.pem" ]; then
  NATS_PUBLISH="0.0.0.0:4222"
  NATS_TLS_BLOCK=$(cat <<'EOF'

# TLS on the client port (ADR-0089). The files come from secrets/tls, which ADR-0090 made the one
# certificate path; tls-export.sh refreshes them on renewal and SIGHUPs this server, which re-reads
# its certificates on that signal. No client-certificate verification yet — there is no `verify`
# directive here and the default is off, so a store authenticates with the token above. Per-store
# client certificates are their own slice; when they land this block gains `verify_and_map: true`
# and a `ca_file` pointing at a **private** client CA, never a public one (ADR-0089, debate D25:
# a public CA there means anyone who can get a certificate from it can speak to the bus).
tls {
  cert_file: "/etc/nats/tls/fullchain.pem"
  key_file:  "/etc/nats/tls/privkey.pem"
  timeout:   5
}
EOF
)
else
  NATS_PUBLISH="127.0.0.1:4222"
  NATS_TLS_BLOCK=""
fi

cat > "$SECRETS/nats.conf" <<EOF
# NATS server config (generated by bootstrap.sh on every run; the token is carried across). Never
# commit. The client port is TLS-wrapped and published only when secrets/tls holds a certificate.
http_port: 8222
jetstream {
  store_dir: "/data"
}
authorization {
  token: "$NATS_TOKEN"
}$NATS_TLS_BLOCK
EOF
chmod 600 "$SECRETS/nats.conf"
echo "write  nats.conf ($NATS_TOKEN_STATE token, client port $NATS_PUBLISH$([ -n "$NATS_TLS_BLOCK" ] && echo ", TLS on" || echo ", TLS off"))"
if [ -z "$NATS_TLS_BLOCK" ]; then
  echo "note   the event bus stays closed to the internet: no certificate in $SECRETS/tls yet."
  case "$CADDY_TLS_MODE" in
    acme-http01 | acme-dns01)
      echo "note   on the ACME modes tls-export.sh publishes it once Caddy has issued; the next bootstrap (a redeploy, or its cron run) opens the port. Expected on a first deploy."
      ;;
    byo-cert) echo "note   TLS_MODE=byo-cert: the certificate should be there. Check secrets/tls/." ;;
    external) echo "note   TLS_MODE=external issues no certificate here, so the bus needs one brought to secrets/tls/ before it can open (ADR-0090)." ;;
    *) ;;
  esac
fi

# The bucket OTA release artifacts live in, and the name of the key that reaches it (ADR-0088).
GARAGE_BUCKET="${GARAGE_BUCKET:-pos-artifacts}"
GARAGE_KEY_NAME="${GARAGE_KEY_NAME:-pos-cloud}"

# 4. Garage single node. The rpc_secret is pre-generatable; the S3 access keys are not —
#    Garage mints those at runtime with `garage key create` when the bucket and layout are
#    set up, which lands with backups (P8d).
if [ ! -e "$SECRETS/garage.toml" ]; then
  cat > "$SECRETS/garage.toml" <<EOF
# Garage single-node config (generated by bootstrap.sh; ADR-0031). Never commit.
metadata_dir = "/var/lib/garage/meta"
data_dir = "/var/lib/garage/data"
db_engine = "lmdb"
replication_factor = 1
rpc_bind_addr = "[::]:3901"
rpc_secret = "$(rand_hex 32)"

[s3_api]
s3_region = "garage"
api_bind_addr = "[::]:3900"
root_domain = ".s3.garage.local"
EOF
  chmod 600 "$SECRETS/garage.toml"
  echo "create garage.toml"
else
  echo "keep   garage.toml"
fi

# 5. Install the Caddyfile for the resolved posture, and the certificate directory every consumer
#    reads (ADR-0090). Nothing here overwrites a version-controlled file: the four per-mode files
#    under ./Caddyfile.d/ are committed and read-only to this script, and the *generated* copy lands
#    in ./secrets/. That is why the mode is legible from a filename instead of only from content.
MODE_FILE="$HERE/Caddyfile.d/$CADDY_TLS_MODE.caddy"
[ -e "$MODE_FILE" ] || { echo "missing $MODE_FILE for TLS_MODE=$CADDY_TLS_MODE" >&2; exit 1; }

mkdir -p "$SECRETS/tls"
chmod 700 "$SECRETS/tls"
if [ "$CADDY_TLS_MODE" = "byo-cert" ]; then
  # Their presence was already required in step 1b, before anything was generated. Here we only fix
  # the permissions: a brought private key arrives with whatever mode the operator's copy left.
  chmod 644 "$SECRETS/tls/fullchain.pem"
  chmod 600 "$SECRETS/tls/privkey.pem"
fi

cp "$MODE_FILE" "$SECRETS/Caddyfile"
chmod 644 "$SECRETS/Caddyfile"
echo "caddy  TLS_MODE=$CADDY_TLS_MODE for ${CADDY_DOMAIN:-unset} (from Caddyfile.d/$CADDY_TLS_MODE.caddy)"

# 5b. The Compose variables the posture implies, written to ../.env beside compose.yml. Compose
#     reads that file automatically, so a later `docker compose up -d` typed by hand reproduces the
#     same posture — without it, the port publishes would silently revert to their defaults and an
#     internet-facing :443 would reappear on a cell whose operator believes TLS terminates upstream.
#     Not a secret: it holds port bindings and image tags. Still git-ignored, since it is generated.
case "$CADDY_TLS_MODE" in
  external)
    # 443 becomes loopback-only rather than absent: Compose cannot conditionally omit a port, and a
    # 127.0.0.1 publish is the honest equivalent of not offering one. The HTTP publish is a variable
    # because a box already running the company's own proxy will not have :80 free.
    CADDY_HTTP_PUBLISH="0.0.0.0:${EXTERNAL_HTTP_PORT:-80}"
    CADDY_HTTPS_PUBLISH="127.0.0.1:443"
    ;;
  *)
    CADDY_HTTP_PUBLISH="0.0.0.0:80"
    CADDY_HTTPS_PUBLISH="0.0.0.0:443"
    ;;
esac
{
  echo "# Generated by bootstrap.sh from TLS_MODE=$CADDY_TLS_MODE (ADR-0090). Not a secret; not committed."
  echo "# Compose reads this automatically, so a hand-typed \`docker compose up -d\` keeps this posture."
  echo "CADDY_HTTP_PUBLISH=$CADDY_HTTP_PUBLISH"
  echo "CADDY_HTTPS_PUBLISH=$CADDY_HTTPS_PUBLISH"
  # The event bus's client port, decided in step 3: internet-facing only when TLS is on, loopback
  # otherwise. compose.yml defaults to loopback, so a tree with no ./.env keeps the port closed.
  echo "NATS_PUBLISH=$NATS_PUBLISH"
  # Pin the images the deploy just loaded, when it told us which. Without this a manual bring-up
  # falls back to `:local` tags that may not exist and Compose rebuilds from source on the box.
  # Written as `if`, not `[ … ] && echo`: as the last command in this group a false test would make
  # the group exit non-zero and `set -e` would abort the bootstrap.
  if [ -n "${POS_CLOUD_IMAGE:-}" ]; then echo "POS_CLOUD_IMAGE=$POS_CLOUD_IMAGE"; fi
  if [ -n "${CADDY_IMAGE:-}" ]; then echo "CADDY_IMAGE=$CADDY_IMAGE"; fi
} > "$HERE/.env"
chmod 644 "$HERE/.env"
echo "write  .env (http=$CADDY_HTTP_PUBLISH https=$CADDY_HTTPS_PUBLISH)"

# 6. Announce the one-time super-admin setup token (ADR-0045). It was captured into SETUP_TOKEN
#    when cloud.toml was created above (step 2), so we do not re-read the file — which the chown
#    may have made unreadable to a non-root deploy user. On a re-run SETUP_TOKEN is unset (the
#    file was kept), so nothing is printed; recover it with `sudo sed -n … secrets/cloud.toml`.
if [ "$CLOUD_TOML_CREATED" = "1" ] && [ -n "${SETUP_TOKEN:-}" ]; then
  echo
  echo "================= ONE-TIME SUPER-ADMIN SETUP TOKEN ================="
  echo "  $SETUP_TOKEN"
  echo "  Enrol the first super-admin with it at POST /admin/setup, then it is void."
  echo "==================================================================="
  echo
else
  echo "keep   super-admin setup token (in secrets/cloud.toml; void once the first admin is enrolled)"
fi

# 7. Bring the stack up (idempotent). Skipped with POS_BOOTSTRAP_NO_UP=1 or no docker.
if [ "${POS_BOOTSTRAP_NO_UP:-0}" = "1" ]; then
  echo "skip   compose up (POS_BOOTSTRAP_NO_UP=1); run: docker compose -f \"$COMPOSE\" up -d --build"
elif command -v docker >/dev/null 2>&1; then
  if [ "${POS_BOOTSTRAP_NO_BUILD:-0}" = "1" ]; then
    # The deploy workflow sets this and POS_CLOUD_IMAGE/CADDY_IMAGE to the tags it just loaded,
    # so the box runs the prebuilt images and never rebuilds pos_cloud from source.
    echo "up     docker compose up -d (prebuilt images)"
    docker compose -f "$COMPOSE" up -d
  else
    echo "up     docker compose up -d --build"
    docker compose -f "$COMPOSE" up -d --build
  fi
else
  echo "note   docker not found; when installed, run: docker compose -f \"$COMPOSE\" up -d --build"
fi

# 7b. Wire Garage for OTA release artifacts, and record the credentials in cloud.toml (ADR-0088).
#
#     Garage mints its own S3 access keys — unlike every other secret here, they cannot be generated
#     ahead of time with `openssl rand`, because the server has to know them too. That is the *only*
#     part of this that is special. It is not a step for a person: this script already runs on the
#     box on every deploy, so it creates the layout, the bucket and the key itself.
#
#     Everything below is idempotent. `garage layout apply` is one-shot per version, and the bucket
#     and key already exist on a redeploy, so each step checks first and reports "keep" rather than
#     failing. A run that cannot reach Garage leaves cloud.toml untouched and says so: the cloud
#     boots fine without an [artifacts] block, with the OTA route off.
if [ "${POS_BOOTSTRAP_NO_UP:-0}" != "1" ] && command -v docker >/dev/null 2>&1; then
  garage_cli() { docker compose -f "$COMPOSE" exec -T garage /garage "$@" 2>/dev/null; }

  if grep -q '^\[artifacts\]' "$SECRETS/cloud.toml" 2>/dev/null; then
    echo "keep   garage artifact credentials (already in cloud.toml)"
  else
    # Garage needs a moment after `up` before its RPC answers.
    garage_ready=0
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      if garage_cli status >/dev/null; then garage_ready=1; break; fi
      sleep 2
    done

    if [ "$garage_ready" != "1" ]; then
      echo "warn   garage did not answer; [artifacts] not written, so the OTA artifact route stays off"
    else
      # A single-node layout, assigned once. `garage status` prints the node id in its first column;
      # `layout assign` is a no-op message on a node that already has a role, so the guard is the
      # layout version rather than the assign itself.
      node_id="$(garage_cli status | awk '/^[0-9a-f]{16}/ { print $1; exit }')"
      if [ -z "$node_id" ]; then
        echo "warn   could not read the garage node id; [artifacts] not written"
      else
        if garage_cli layout show | grep -q 'NO ROLE\|No nodes'; then
          garage_cli layout assign -z pos -c 10G "$node_id" >/dev/null || true
          garage_cli layout apply --version 1 >/dev/null || true
          echo "create garage layout (single node, zone pos)"
        else
          echo "keep   garage layout"
        fi

        if garage_cli bucket info "$GARAGE_BUCKET" >/dev/null; then
          echo "keep   garage bucket $GARAGE_BUCKET"
        else
          garage_cli bucket create "$GARAGE_BUCKET" >/dev/null || true
          echo "create garage bucket $GARAGE_BUCKET"
        fi

        # `key create` prints the id and secret once. They are captured here and written straight
        # into cloud.toml (mode 600) — the same place the database password lives, and never echoed.
        key_out="$(garage_cli key create "$GARAGE_KEY_NAME")"
        key_id="$(printf '%s\n' "$key_out" | awk -F': *' '/Key ID/ { print $2; exit }')"
        key_secret="$(printf '%s\n' "$key_out" | awk -F': *' '/Secret key/ { print $2; exit }')"

        if [ -z "$key_id" ] || [ -z "$key_secret" ]; then
          echo "warn   could not mint a garage key; [artifacts] not written"
        else
          garage_cli bucket allow --read --write "$GARAGE_BUCKET" --key "$key_id" >/dev/null || true
          {
            echo ""
            echo "# Where OTA release artifacts live (ADR-0088). Written by bootstrap.sh: Garage mints"
            echo "# its own S3 keys, so unlike the secrets above these are captured rather than generated."
            echo "[artifacts]"
            echo "endpoint = \"http://garage:3900\""
            echo "bucket = \"$GARAGE_BUCKET\""
            echo "region = \"garage\""
            echo "access_key_id = \"$key_id\""
            echo "secret_access_key = \"$key_secret\""
          } >> "$SECRETS/cloud.toml"
          echo "create garage artifact credentials (in secrets/cloud.toml)"
        fi
      fi
    fi
  fi
fi

# 8. Publish the certificate to secrets/tls on the two ACME modes (ADR-0090), so the path exists
#    from the first deploy instead of only after cron's first pass. Best-effort and non-fatal: on a
#    first bring-up ACME has usually not finished yet, and "not issued yet" is the normal answer.
#    Add the cron line from docs/deploy-runbook.md so renewals keep the exported copy current — a
#    consumer that reads a stale certificate fails only when the old one expires, weeks later.
case "$CADDY_TLS_MODE" in
  acme-http01 | acme-dns01)
    chmod +x "$HERE/tls-export.sh" 2>/dev/null || true
    if "$HERE/tls-export.sh"; then
      :
    else
      echo "note   tls-export.sh did not export yet (normal on a first deploy, before ACME issues); cron picks it up"
    fi
    ;;
  *) ;;
esac

echo "done   bootstrap complete (TLS_MODE=$CADDY_TLS_MODE, trusted_proxy_hops=$TRUSTED_PROXY_HOPS)"
