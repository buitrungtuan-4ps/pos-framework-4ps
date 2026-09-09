#!/usr/bin/env bash
# deploy/reset-admin.sh — the super-admin break-glass (ADR-0045).
#
# Wipes every console admin identity and credential, plus every live session, so the first-boot
# enrolment route (POST /admin/setup) can run again — the recovery path when the authenticator is
# lost. Run on the box by the deploy workflow's reset_admin path, itself gated by the `production`
# Environment's required reviewer (the second human the break-glass needs). Idempotent and safe
# before the schema exists: each DELETE is guarded by to_regclass, so on an already-empty table it
# deletes nothing, and if the admin tables have not been created yet — a reset_admin run before the
# app's first migration, e.g. ticked on a first deploy — it is a clean no-op rather than an error.
#
# `admin_users` is cleared because since
# [ADR-0119](../docs/adr/0119-each-admin-signs-in-as-themselves.md) that table holds the credential
# sign-in reads. Clearing only `super_admin` — which is what this script did while that was the
# authority — would now delete a credential nothing reads and leave the one that matters in place:
# the operator would be locked out with the break-glass already spent. Removing every admin, not
# only the owner, is deliberate: the last resort returns the installation to first boot, and an
# invited admin whose own credential survived would be a way back in that the operator reaching for
# this script cannot see or audit.
#
# Reads the database credentials from ./secrets/pos.env (never the environment), so it works
# whatever the container's pg_hba allows.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"

# shellcheck source=/dev/null
set -a
. "$HERE/secrets/pos.env"
set +a

# to_regclass returns NULL for a table that does not exist, so each DELETE is skipped when the
# admin schema is not there yet — a break-glass before the app's first migration is a no-op, not
# an error. The heredoc is quoted (<<'SQL') so the shell does not touch the `$$` dollar-quoting.
docker compose -f "$HERE/compose.yml" exec -T \
  -e PGPASSWORD="$POSTGRES_PASSWORD" postgres \
  psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -v ON_ERROR_STOP=1 <<'SQL'
DO $$
BEGIN
  IF to_regclass('public.super_admin')    IS NOT NULL THEN DELETE FROM super_admin;    END IF;
  IF to_regclass('public.admin_sessions') IS NOT NULL THEN DELETE FROM admin_sessions; END IF;
  -- Order matters: `admin_invites.invited_by` references `admin_users (id)` with no cascade, so a
  -- pending invitation would block the delete below. Recovery codes and sessions do cascade, but
  -- sessions are cleared above anyway so a reset before the 0018 migration still wipes them.
  IF to_regclass('public.admin_invites')  IS NOT NULL THEN DELETE FROM admin_invites;  END IF;
  IF to_regclass('public.admin_users')    IS NOT NULL THEN DELETE FROM admin_users;    END IF;
END $$;
SQL

echo "every admin, invitation and session cleared; re-enrol at POST /admin/setup with the setup token"
