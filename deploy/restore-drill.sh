#!/usr/bin/env bash
# deploy/restore-drill.sh — the restore drill (P8d, ADR-0046).
#
# Proves the cloud-database backup is not just written but *loadable*: dump the live database,
# restore it into a throwaway database, and reconcile every public table's row count against the
# source. A failed restore, or any count that does not match, exits non-zero — because a backup
# that has never been restored is not a backup, and the drill is the thing that catches a silently
# unrestorable one.
#
# Connects over libpq (PGHOST / PGPORT / PGUSER / PGPASSWORD / PGDATABASE), so the same script
# serves the nightly CI drill (a service Postgres) and a box cron pointed at a reachable server.
#
# The archive requires the drill cover BOTH the cloud database and a random store backup. The
# store half is edge WAL shipping (docs/roadmap.md P9, spike A4); this is the cloud half, and the
# drill grows its store half when that lands.
set -euo pipefail

: "${PGHOST:?set PGHOST/PGPORT/PGUSER/PGPASSWORD/PGDATABASE for the drill}"
: "${PGDATABASE:?set PGDATABASE — the cloud database to drill}"
scratch="${DRILL_DATABASE:-poscloud_drill}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
dump="${work}/drill.dump"

echo "drill    dumping ${PGDATABASE}"
pg_dump -Fc -f "$dump"

echo "drill    restoring into ${scratch}"
# Connect to the maintenance database for the DROP/CREATE (you cannot drop the database you are
# connected to). Drop any leftover scratch from an interrupted run first.
psql -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${scratch}" \
  -c "CREATE DATABASE ${scratch}"
# --no-owner/--no-privileges: the scratch database is owned by the drill user, so roles and grants
# from the source are irrelevant to proving the data restores.
pg_restore --no-owner --no-privileges -d "$scratch" "$dump"

echo "drill    reconciling row counts"
mismatch=0
tables="$(psql -Atc "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename")"
for table in $tables; do
  live="$(psql -Atc "SELECT count(*) FROM public.\"${table}\"")"
  restored="$(psql -d "$scratch" -Atc "SELECT count(*) FROM public.\"${table}\"")"
  if [ "$live" = "$restored" ]; then
    echo "  ok    ${table}: ${live}"
  else
    echo "  FAIL  ${table}: live=${live} restored=${restored}" >&2
    mismatch=1
  fi
done

echo "drill    dropping ${scratch}"
psql -d postgres -c "DROP DATABASE IF EXISTS ${scratch}" >/dev/null

if [ "$mismatch" -ne 0 ]; then
  echo "drill    FAILED: restored data does not reconcile with the source" >&2
  exit 1
fi
echo "drill    OK: every public table reconciled between source and restore"
