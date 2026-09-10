#!/usr/bin/env bash
# deploy/store-restore-drill.sh — the *store* leg of the restore drill
# (ADR-0046, ADR-0124 slice 3).
#
# `restore-drill.sh` proves the **cloud database** restores. This proves a **store's** archive
# does: pull the newest sealed archive a shop has shipped, open it with that store's key, and run
# SQLite's own integrity check over what came out. Exits non-zero if anything in that chain fails —
# because a backup that has never been restored is not a backup, and this is the thing that catches
# an archive that has been landing nightly for six months and cannot be opened.
#
# It is deliberately narrow. It answers "can this store be brought back", and nothing else.
#
# USAGE
#
#   POS_EDGE_ARCHIVE_KEY=<64 hex characters> \
#     deploy/store-restore-drill.sh --store <STORE_ULID> [--archive <path>] [--remote <rclone:path>]
#
#   --store    Required. The store the archive belongs to. An archive is bound to its store, so the
#              wrong id refuses rather than restoring the wrong shop.
#   --archive  A sealed archive already on this machine. Skips the download.
#   --remote   An `rclone` remote and prefix holding the store's archives, e.g.
#              `offsite:pos-archives/stores/<STORE_ULID>/archives`. The newest object under it is
#              the one drilled — `taken_at` is zero-padded in the object name, so lexical order is
#              chronological order and `sort | tail -1` is the newest.
#
# THE KEY IS NEVER AN ARGUMENT
#
# It comes from `POS_EDGE_ARCHIVE_KEY`, like every other use of `pos-edge archive`: a key on the
# command line is a key in `ps` output and in the operator's shell history. Fetch it from the
# console (which holds it wrapped) into the environment for the length of this run and no longer.
#
# WHY IT IS NOT IN THE NIGHTLY LANE
#
# It needs three things CI does not have: a real store's archive, that store's key, and credentials
# for the tier the archives sync to. The *format* is drilled automatically on every pull request —
# `crates/pos-edge/tests/store_archive.rs` seals a till that has traded and restores it on another
# machine, and `crates/pos-edge/tests/backup_loop.rs` does the same through the shipping loop. What
# this script adds is the leg those cannot reach: the archive a *particular shop actually shipped*,
# through the tier it actually travelled. Run it on the schedule the operator keeps for the cloud
# drill, against a different store each time.
set -euo pipefail

store=""
archive=""
remote=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --store)   store="${2:?--store needs a value}"; shift 2 ;;
    --archive) archive="${2:?--archive needs a value}"; shift 2 ;;
    --remote)  remote="${2:?--remote needs a value}"; shift 2 ;;
    *) echo "drill    unknown argument: $1" >&2; exit 2 ;;
  esac
done

: "${store:?--store <STORE_ULID> is required}"
: "${POS_EDGE_ARCHIVE_KEY:?set POS_EDGE_ARCHIVE_KEY to the 64-character key for this store}"
pos_edge="${POS_EDGE_BIN:-pos-edge}"
command -v "$pos_edge" >/dev/null 2>&1 || {
  echo "drill    ${pos_edge} is not on PATH; set POS_EDGE_BIN to the binary" >&2
  exit 2
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [ -z "$archive" ]; then
  : "${remote:?give either --archive <path> or --remote <rclone:prefix>}"
  echo "drill    listing ${remote}"
  # Newest by name, which is newest by time: `taken_at` is zero-padded in the object key precisely
  # so this holds without parsing anything.
  newest="$(rclone lsf "$remote" | grep -E '\.p4p$' | sort | tail -1)"
  [ -n "$newest" ] || { echo "drill    FAILED: ${remote} holds no archive for this store" >&2; exit 1; }
  echo "drill    fetching ${newest}"
  rclone copyto "${remote}/${newest}" "${work}/archive.p4p"
  archive="${work}/archive.p4p"
fi

[ -f "$archive" ] || { echo "drill    FAILED: no such archive: ${archive}" >&2; exit 1; }
echo "drill    verifying $(basename "$archive") for store ${store}"

# `archive verify` is the whole drill: it opens the archive under the key, writes the database out
# beside it, runs `PRAGMA integrity_check`, and removes the plaintext copy either way. A wrong key,
# another shop's archive, one altered byte and a truncated download are all one refusal — an
# authenticated cipher cannot tell them apart, and a message that guessed would mislead.
if "$pos_edge" archive verify --store "$store" --archive "$archive"; then
  echo "drill    OK: store ${store} can be restored from this archive"
else
  echo "drill    FAILED: the archive for store ${store} did not open, or the database inside is damaged" >&2
  echo "drill    check the key first — it is the likeliest cause and the cheapest to rule out" >&2
  exit 1
fi
