#!/usr/bin/env bash
# Copyright (c) 2026 Pizza 4P's. All rights reserved.
# Proprietary and confidential. Internal use only. See LICENSE.
#
# The cloud's environment key under systemd-creds (ADR-0217 decision 5, as ADR-0151 does for the
# edge). Run as root on the cloud's host.
#
#   cloud-keys.sh new      seals a new environment-key version: 32 random bytes from the kernel,
#                          encrypted by systemd-creds to the host's TPM2 when it has one (else to
#                          the host key only root reads), into /etc/credstore.encrypted. Nobody
#                          sees or types the key. Run `unseal`, restart pos_cloud, then
#                          `docker compose exec pos_cloud pos-cloud keys rotate --environment`.
#   cloud-keys.sh unseal   decrypts every version into /run/pos-cloud/credentials (tmpfs, gone at
#                          reboot), readable by the pos_cloud container's uid 10001 only. Run
#                          before every start, e.g. as an ExecStartPre of the unit that starts
#                          compose; the container mounts it read-only (deploy/README.md).
#
# A version is never removed here: one is retired only once nothing is wrapped under it.
set -euo pipefail

STORE=/etc/credstore.encrypted
RUN_DIR=/run/pos-cloud/credentials
PREFIX=pos-cloud-environment-key-v
CLOUD_UID=10001

newest() {
  local n=0 f v
  for f in "$STORE/$PREFIX"*; do
    [ -e "$f" ] || continue
    v=${f##*"$PREFIX"}
    [[ "$v" =~ ^[0-9]+$ ]] && [ "$v" -gt "$n" ] && n=$v
  done
  echo "$n"
}

case "${1:-}" in
  new)
    install -d -m 0700 "$STORE"
    next=$(( $(newest) + 1 ))
    head -c 32 /dev/urandom \
      | systemd-creds encrypt --with-key=auto --name="$PREFIX$next" - "$STORE/$PREFIX$next"
    chmod 0600 "$STORE/$PREFIX$next"
    echo "sealed environment key v$next"
    ;;
  unseal)
    install -d -m 0750 -o root -g "$CLOUD_UID" "$RUN_DIR"
    for f in "$STORE/$PREFIX"*; do
      [ -e "$f" ] || continue
      name=${f##*/}
      systemd-creds decrypt --name="$name" "$f" "$RUN_DIR/$name"
      chown root:"$CLOUD_UID" "$RUN_DIR/$name"
      chmod 0440 "$RUN_DIR/$name"
    done
    echo "unsealed $(find "$RUN_DIR" -name "$PREFIX*" | wc -l) environment key version(s)"
    ;;
  *)
    echo "usage: $0 new|unseal" >&2
    exit 2
    ;;
esac
