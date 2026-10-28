#!/usr/bin/env bash
# deploy/backup.sh — cloud database backup (P8d, ADR-0046).
#
# Streams a compressed pg_dump of the cloud database out of the compose Postgres to a local
# file, then ships it off-box with rclone if RCLONE_REMOTE is set — a backup that lives only on
# the database's own box is not a backup. Reads the database credentials from ./secrets/pos.env.
# Idempotent and safe to re-run.
#
# Backup classes (ADR-0046):
#   --label scheduled   the daily floor (the default)
#   --label pre-update  the snapshot the deploy workflow takes before bringing up a new image
# Continuous WAL archiving is configured on the Postgres service itself (compose.yml), not here.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
label="scheduled"
out_dir="${BACKUP_DIR:-$HERE/../backups}"

while [ $# -gt 0 ]; do
  case "$1" in
    --label) label="${2:?--label needs a value}"; shift 2 ;;
    --out)   out_dir="${2:?--out needs a value}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# shellcheck source=/dev/null
set -a
. "$HERE/secrets/pos.env"
set +a

mkdir -p "$out_dir"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
file="${out_dir}/poscloud-${label}-${stamp}.dump"

# -Fc: custom format — compressed and selectively restorable with pg_restore. -T on exec so the
# binary dump streams to the host file uncorrupted by a pseudo-TTY.
docker compose -f "$HERE/compose.yml" exec -T -e PGPASSWORD="$POSTGRES_PASSWORD" postgres \
  pg_dump -Fc -U "$POSTGRES_USER" "$POSTGRES_DB" > "$file"
echo "backup   $file"

# Where the off-box copy rests, against the cell's home country (ADR-0179 C7, C8). bootstrap.sh
# records both in ../.env from deploy/cells/<cell>.env; the environment may override either. A
# difference is a warning and never stops a backup: an unshipped backup protects nothing.
# `|| true`: under `set -euo pipefail` a missing ../.env would otherwise end the script here, after
# the dump and before the off-box copy, which is the one outcome this file exists to prevent.
env_value() { sed -n "s/^$1=//p" "$HERE/.env" 2>/dev/null | tail -1 || true; }
BACKUP_COUNTRY="${BACKUP_COUNTRY:-$(env_value BACKUP_COUNTRY)}"
CELL_HOME_COUNTRY="${CELL_HOME_COUNTRY:-$(env_value CELL_HOME_COUNTRY)}"
backup_region_note() {
  if [ -z "${BACKUP_COUNTRY:-}" ]; then
    echo "warn     the country these backups rest in is not recorded: set BACKUP_COUNTRY in deploy/cells/<cell>.env (ADR-0179)"
  elif [ -n "${CELL_HOME_COUNTRY:-}" ] && [ "$BACKUP_COUNTRY" != "$CELL_HOME_COUNTRY" ]; then
    echo "warn     backups rest in $BACKUP_COUNTRY, outside this cell's home country $CELL_HOME_COUNTRY: record why that transfer is covered (ADR-0179)"
  else
    echo "region   backups rest in $BACKUP_COUNTRY"
  fi
}

if [ -n "${RCLONE_REMOTE:-}" ]; then
  rclone copy "$file" "${RCLONE_REMOTE%/}/cloud-db/"
  echo "shipped  ${RCLONE_REMOTE%/}/cloud-db/$(basename "$file")"
  backup_region_note
else
  echo "note     RCLONE_REMOTE unset; kept on-box only — set it for the off-box tier (ADR-0046)"
fi
