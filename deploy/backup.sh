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

if [ -n "${RCLONE_REMOTE:-}" ]; then
  rclone copy "$file" "${RCLONE_REMOTE%/}/cloud-db/"
  echo "shipped  ${RCLONE_REMOTE%/}/cloud-db/$(basename "$file")"
else
  echo "note     RCLONE_REMOTE unset; kept on-box only — set it for the off-box tier (ADR-0046)"
fi
