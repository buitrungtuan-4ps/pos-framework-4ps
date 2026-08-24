#!/usr/bin/env bash
# deploy/reset-admin.sh — the super-admin break-glass (ADR-0045).
#
# Wipes the single super-admin credential and every live session, so the first-boot enrolment
# route (POST /admin/setup) can run again — the recovery path when the authenticator is lost.
# Run on the box by the deploy workflow's reset_admin path, itself gated by the `production`
# Environment's required reviewer (the second human the break-glass needs). Idempotent: on an
# already-empty table it deletes nothing and still succeeds.
#
# Reads the database credentials from ./secrets/pos.env (never the environment), so it works
# whatever the container's pg_hba allows.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"

# shellcheck source=/dev/null
set -a
. "$HERE/secrets/pos.env"
set +a

docker compose -f "$HERE/compose.yml" exec -T \
  -e PGPASSWORD="$POSTGRES_PASSWORD" postgres \
  psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
  -c 'DELETE FROM super_admin; DELETE FROM admin_sessions;'

echo "super-admin and all sessions cleared; re-enrol at POST /admin/setup with the setup token"
