#!/usr/bin/env bash
# Copyright (c) 2026 Pizza 4P's. All rights reserved.
# Proprietary and confidential. Internal use only. See LICENSE.
#
# deploy/appliance/provision.sh — turns a stock Debian 12 or Ubuntu 24.04 install into a store box
# (ADR-0150). The operator's guide is docs/guides/appliance.md; `--help` is the short form.
#
# WHAT IT DOES, in order. Every step is safe to repeat, and `--dry-run` prints them instead:
#
#   1. creates `pos`, the unprivileged account deploy/edge/pos-edge.service runs the store as;
#   2. puts the binary in the update slots the edge's own updater manages (bin/slot-a, with
#      bin/current pointing at it: ADR-0055 Amendment 1) and the operator's rescue copy at
#      /usr/local/bin/pos-edge. Only once: a box that already has bin/current is running whatever it
#      updated itself to, and re-laying slot-a would silently downgrade it;
#   3. installs fonts-dejavu-core, without which receipts print ASCII only (deploy/edge/README.md);
#   4. installs deploy/edge/pos-edge.service, unchanged, and, where systemd-creds is available,
#      deploy/edge/pos-edge-vault with the unit drop-in that runs it, so the device credential is
#      sealed and survives a reboot (ADR-0151). The key is sealed at the machine's first start;
#   5. then EITHER, with --store, writes config.toml and enables and starts the edge; OR, without it,
#      installs pos-edge-claim.service, which runs `pos-edge claim` on first boot so that the console
#      picks the store (ADR-0148), then starts the edge. The second is how one image serves every
#      store: it carries no config.toml, and a stolen copy claims nothing;
#   6. with --kiosk, adds a cage + Chromium session on tty1 that opens the till full screen, from
#      the next boot. No display manager.
#
# THE LAYOUT IS NOT NEW. It is the one the console's installer lays out (linuxInstaller in
# dashboard/src/installers.mjs) and deploy/edge/pos-edge.service documents: the same user, paths,
# owners and modes. A second definition of where a store keeps its binary would be one the updater
# does not find. dashboard/scripts/installer-syntax.mjs holds this file to that: the unit embedded
# below must match deploy/edge/pos-edge.service, the vault helper and drop-in must match their files
# in deploy/edge, and the config.toml written here must match the console's, or the dashboard build
# fails.
#
# It never takes a secret on its command line, where every process on the box can read it. The
# device credential arrives through activation or the claim; the env file is filled in by hand.

set -euo pipefail
# Byte-for-byte text handling, and English from apt and ufw, whose output is read below.
export LC_ALL=C

readonly SERVICE_USER=pos
readonly STATE=/var/lib/pos-edge
readonly CONFIG=$STATE/config.toml
readonly BIN=$STATE/bin
readonly RESCUE=/usr/local/bin/pos-edge
readonly ETC=/etc/pos-edge
readonly UNITS=/etc/systemd/system
readonly VAULT_HELPER=/usr/local/libexec/pos-edge/pos-edge-vault
# pos_edge's DEFAULT_BIND port (crates/pos-edge/src/config.rs).
readonly DEFAULT_PORT=8787

readonly KIOSK_USER=pos-kiosk
readonly KIOSK_LAUNCHER=/usr/local/libexec/pos-kiosk
readonly KIOSK_PAM=/etc/pam.d/pos-kiosk

BINARY=""
CLOUD=""
STORE=""
KIOSK=false
DRY_RUN=false
OS=""
APT_UPDATED=false

usage() {
  cat <<'USAGE'
Usage: sudo provision.sh --binary <pos-edge> --cloud <https://host> [--store <ULID>] [--kiosk]
       provision.sh --dry-run ...

Turns a stock Debian 12 or Ubuntu 24.04 install into a store box (ADR-0150).

  --binary <path>  the pos-edge release binary for this machine's architecture. Needed on the first
                   run; later runs leave the installed binary alone, because the edge updates itself.
  --cloud <url>    the cloud this box dials: an https origin such as https://cloud.example.com.
  --store <ULID>   the store this box is. config.toml is written now and the edge starts. Without
                   it the box claims itself on first boot: it shows a code, and the console picks
                   the store under Activation, Claim a box (ADR-0148).
  --kiosk          also open the till full screen on this machine's display (cage and Chromium on
                   tty1), from the next boot.
  --dry-run        print every action instead of taking it. Needs no root.
  -h, --help       this text.

Safe to run again: it re-applies the same layout, never replaces the binary the edge is running, and
never overwrites config.toml or /etc/pos-edge/env. Guide: docs/guides/appliance.md
USAGE
}

say() { printf '==> %s\n' "$*"; }
warn() { printf 'provision.sh: warning: %s\n' "$*" >&2; }
die() {
  printf 'provision.sh: %s\n' "$*" >&2
  exit 1
}
usage_error() {
  printf 'provision.sh: %s (see --help)\n' "$*" >&2
  exit 2
}

# Runs a command, or under --dry-run prints it. Every change this script makes goes through here or
# through write_file, which is what makes a dry run a faithful list of what a real run does.
run() {
  if [ "$DRY_RUN" = true ]; then
    printf '+'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

# write_file <path> <mode> <owner:group> [quiet] < content
#
# Writes stdin to <path> through a temporary file in the same directory and one rename, so nothing
# ever reads it half-written. A dry run prints the content instead, unless `quiet` says it is a copy
# of a file already in the repository.
write_file() {
  local path=$1 mode=$2 owner=$3 show=${4:-show} content tmp
  content=$(
    cat
    printf x
  )
  content=${content%x}
  if [ "$DRY_RUN" = true ]; then
    printf '+ write %s (mode %s, %s)\n' "$path" "$mode" "$owner"
    if [ "$show" = show ]; then
      printf '%s' "$content" | sed 's/^/    | /'
    fi
    return 0
  fi
  tmp=$(mktemp "$path.XXXXXX")
  printf '%s' "$content" >"$tmp"
  chown "$owner" "$tmp"
  chmod "$mode" "$tmp"
  mv -f "$tmp" "$path"
}

# Whether systemd is this machine's init and running: false in a chroot, which is how an image is
# built, and in a container without systemd. Units are then enabled but not started.
systemd_running() { [ -d /run/systemd/system ]; }

# Where this script lives, to find deploy/edge/pos-edge.service beside it in a checkout.
script_dir() {
  local source=${BASH_SOURCE[0]:-$0}
  cd "$(dirname -- "$source")" && pwd
}

# Prints a top-level string value from config.toml (`store_id`, `cloud_url`, `bind`), or nothing.
config_value() {
  sed -n '/^[[:space:]]*'"$1"'[[:space:]]*=/{s/^[^"]*"\([^"]*\)".*/\1/p;q;}' "$CONFIG"
}

# The port the edge listens on: config.toml's `bind`, or the default.
edge_port() {
  local bind=""
  if [ -f "$CONFIG" ]; then
    bind=$(config_value bind)
  fi
  bind=${bind:-0.0.0.0:$DEFAULT_PORT}
  printf '%s\n' "${bind##*:}"
}

parse_args() {
  while [ $# -gt 0 ]; do
    case $1 in
      --binary | --cloud | --store)
        [ $# -ge 2 ] || usage_error "$1 needs a value"
        case $1 in
          --binary) BINARY=$2 ;;
          --cloud) CLOUD=$2 ;;
          --store) STORE=$2 ;;
        esac
        shift 2
        ;;
      --kiosk)
        KIOSK=true
        shift
        ;;
      --dry-run)
        DRY_RUN=true
        shift
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      *) usage_error "unknown argument: $1" ;;
    esac
  done
}

validate_args() {
  if [ -n "$CLOUD" ]; then
    # An https origin and nothing else. The edge's cloud transport dials only https, and nothing in
    # it can break out of the TOML string or the unit line it is written into.
    [[ $CLOUD =~ ^https://[A-Za-z0-9.-]+(:[0-9]{1,5})?/?$ ]] ||
      usage_error "--cloud must be an https origin such as https://cloud.example.com, got '$CLOUD'"
    CLOUD=${CLOUD%/}
  fi
  if [ -n "$STORE" ]; then
    # Crockford base32, which has no I, L, O or U; the edge reads either case and so does this.
    [[ $STORE =~ ^[0-7][0-9A-HJKMNP-TV-Za-hjkmnp-tv-z]{25}$ ]] ||
      usage_error "--store must be a store ULID (26 characters, such as 01JBQ9ZK7X8N4M2P6R3T5V7W9Y), got '$STORE'"
    STORE=$(printf '%s' "$STORE" | tr '[:lower:]' '[:upper:]')
    [ -n "$CLOUD" ] || usage_error "--store needs --cloud: config.toml names the store and the cloud it dials"
  fi
}

# Prints `debian` or `ubuntu` for the two supported releases and refuses anything else: the package
# names used below and the kiosk's PAM stack are only known to hold on those two.
detect_os() {
  local file=$1 found
  [ -r "$file" ] || die "cannot read $file, so cannot tell which distribution this is"
  # shellcheck source=/dev/null
  found=$(. "$file" && printf '%s %s' "${ID:-}" "${VERSION_ID:-}")
  case $found in
    "debian 12") echo debian ;;
    "ubuntu 24.04") echo ubuntu ;;
    *) die "$file says this is '${found}'; provision.sh supports Debian 12 and Ubuntu 24.04 only" ;;
  esac
}

# Everything that can refuse, before anything is changed.
preflight() {
  if [ "$DRY_RUN" != true ] && [ "$(id -u)" -ne 0 ]; then
    die "run me as root (sudo), or add --dry-run to see what I would do"
  fi
  OS=$(detect_os /etc/os-release)
  command -v systemctl >/dev/null 2>&1 ||
    die "no systemctl here: the store runs as a systemd service, and this box does not use systemd"
  if [ -n "$BINARY" ]; then
    [ -f "$BINARY" ] || die "no such file: $BINARY"
  elif [ ! -e "$BIN/current" ]; then
    die "no pos-edge is installed here yet: pass --binary <path to the pos-edge release binary>"
  fi
  if [ -f "$CONFIG" ]; then
    local existing
    existing=$(config_value store_id)
    if [ -n "$STORE" ] && [ -n "$existing" ] && [ "$existing" != "$STORE" ]; then
      die "this box is already store $existing ($CONFIG), not $STORE. A box changes store by being wiped, not re-provisioned: its event log belongs to the store that recorded it (docs/guides/appliance.md)"
    fi
  elif [ -z "$CLOUD" ]; then
    usage_error "say which cloud this box dials: --cloud https://... (with --store <ULID> to skip the claim)"
  fi
}

apt_update() {
  if [ "$APT_UPDATED" = false ]; then
    run env DEBIAN_FRONTEND=noninteractive apt-get -q -o DPkg::Lock::Timeout=600 update
    APT_UPDATED=true
  fi
}

# Installs whichever of the named packages are missing. A box that has them all never touches apt,
# so a re-run works offline. The lock timeout is for a first boot, where unattended-upgrades is
# often holding the lock.
apt_install() {
  local missing=() package status
  for package in "$@"; do
    # shellcheck disable=SC2016 # ${Status} is dpkg-query's field syntax, not a shell expansion
    status=$(dpkg-query -W -f='${Status}' "$package" 2>/dev/null || true)
    [ "$status" = "install ok installed" ] || missing+=("$package")
  done
  if [ ${#missing[@]} -eq 0 ]; then
    say "already installed: $*"
    return 0
  fi
  say "installing ${missing[*]}"
  apt_update
  run env DEBIAN_FRONTEND=noninteractive apt-get -q -y -o DPkg::Lock::Timeout=600 install "${missing[@]}"
}

install_service_user() {
  if id -u "$SERVICE_USER" >/dev/null 2>&1; then
    say "service account '$SERVICE_USER' exists"
  else
    say "creating the service account '$SERVICE_USER' (no login shell, no home)"
    run useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
  fi
}

install_binary() {
  run install -d -o "$SERVICE_USER" -g "$SERVICE_USER" "$STATE" "$BIN"
  if [ -e "$BIN/current" ]; then
    say "bin/current exists: leaving the installed binary alone (the edge manages its own updates)"
  else
    say "installing the binary into the first update slot"
    run install -o "$SERVICE_USER" -g "$SERVICE_USER" -m 0755 "$BINARY" "$BIN/slot-a"
    run ln -sfn slot-a "$BIN/current"
    run chown -h "$SERVICE_USER:$SERVICE_USER" "$BIN/current"
  fi
  if [ -n "$BINARY" ]; then
    # The operator's copy, and what `pos-edge --self-test` is run from by hand. Not what runs.
    run install -o root -g root -m 0755 "$BINARY" "$RESCUE"
  fi
}

# deploy/edge/pos-edge.service, verbatim, for a copy of this script run outside a checkout: a
# cloud-init first boot fetches this one file. The file in the checkout is the source of truth, and
# install_edge_unit uses it whenever it is there; installer-syntax.mjs fails the dashboard build the
# moment this copy stops matching it. Change that file, then paste it here.
edge_unit() {
  cat <<'POS_EDGE_SERVICE'
# systemd unit for the store binary (P5).
#
# The edge runs unattended on a store mini-PC. This unit restarts it on crash and on boot, and sends
# SIGTERM on stop — which pos_edge drains gracefully (server.rs), finishing any in-flight request
# before exiting, so "kill mid-sale loses only the uncommitted transaction" holds under a normal
# stop as well as a crash.
#
# Install (as root):
#   install -d -o pos -g pos /var/lib/pos-edge /var/lib/pos-edge/bin
#   install -o pos -g pos -m 0755 pos-edge /var/lib/pos-edge/bin/slot-a
#   ln -sfn slot-a /var/lib/pos-edge/bin/current
#   chown -h pos:pos /var/lib/pos-edge/bin/current
#   cp pos-edge /usr/local/bin/pos-edge          # the operator's copy, for --self-test and rescue
#   cp pos-edge.service /etc/systemd/system/
#   systemctl daemon-reload && systemctl enable --now pos-edge
#
# Why the binary lives under the state directory (ADR-0055 Amendment 1): this unit runs the store as
# an unprivileged user under ProtectSystem=strict and NoNewPrivileges, which between them make the
# whole filesystem read-only to the process except StateDirectory — deliberately, so a compromised
# till cannot replace system binaries. An over-the-air update therefore cannot write
# /usr/local/bin/pos-edge, and must not be given the privilege to. So ExecStart is a symlink the edge
# owns inside its own state, which it retargets with a single atomic rename; the sandbox stays as
# strict as it is. The two slot files alternate, and `previous` is where a rollback goes back to.
#
# A box laid out the old way — ExecStart=/usr/local/bin/pos-edge, no bin/current — keeps trading and
# simply does not self-update; the edge logs that it found no layout and starts no updater.

[Unit]
Description=Pizza 4P's POS edge (store server)
Documentation=https://github.com/buitrungtuan-4ps/pos-framework-4ps
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
# Runs as its own unprivileged user; the store database and config live under StateDirectory.
User=pos
Group=pos
StateDirectory=pos-edge
WorkingDirectory=/var/lib/pos-edge
Environment=POS_EDGE_CONFIG=/var/lib/pos-edge/config.toml
Environment=RUST_LOG=info
# Two credentials the edge reads from the environment, never from config.toml (ADR-0085/0086/0087):
#   POS_EDGE_SYNC_KEY  the scoped read_config + relay_orders key, as a headless bring-up override.
#                      Its durable home is the OS keyring (SecretName::SyncKey).
#   POS_EDGE_NATS_URL  the event stream's server URL, which is where a NATS credential would live.
# Both optional — the leading '-' means the file may be absent. If used: root-owned, mode 0600,
# never committed.
EnvironmentFile=-/etc/pos-edge/env
# The symlink, not a slot: the edge retargets it on a successful install, and systemd
# resolves it afresh on each start (Restart=always is what turns an exit into that start).
ExecStart=/var/lib/pos-edge/bin/current
# SIGTERM triggers graceful shutdown; give in-flight requests time to drain before SIGKILL.
KillSignal=SIGTERM
TimeoutStopSec=30
# A clean exit is how the edge asks to be restarted into a binary it just installed — and how it
# steps back onto the previous one when a new version never reaches a healthy boot. `always`, not
# `on-failure`, is therefore load-bearing: `on-failure` would leave a store stopped after an update.
Restart=always
RestartSec=2

# Hardening: the edge needs no privilege beyond its own state directory.
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
POS_EDGE_SERVICE
}

install_edge_unit() {
  local checkout
  checkout="$(script_dir)/../edge/pos-edge.service"
  if [ -f "$checkout" ]; then
    say "installing pos-edge.service from this checkout (deploy/edge/pos-edge.service)"
    run install -o root -g root -m 0644 "$checkout" "$UNITS/pos-edge.service"
  else
    say "installing pos-edge.service (the copy of deploy/edge/pos-edge.service in this script)"
    edge_unit | write_file "$UNITS/pos-edge.service" 0644 root:root quiet
  fi
}

# deploy/edge/pos-edge-vault and deploy/edge/pos-edge.service.d/vault.conf, verbatim, for a copy of
# this script run outside a checkout (ADR-0151). As with the unit above, the files in the checkout
# are the source of truth and installer-syntax.mjs fails the dashboard build when a copy drifts.
vault_helper() {
  cat <<'POS_EDGE_VAULT'
#!/bin/sh
# Copyright (c) 2026 Pizza 4P's. All rights reserved.
# Proprietary and confidential. Internal use only. See LICENSE.
#
# pos-edge-vault — the root half of a Linux store box's sealed secrets (ADR-0151).
#
# The store runs as the unprivileged `pos` user and keeps its device credential sealed under a
# vault key. Sealing that key needs systemd-creds, and systemd-creds needs root, so this script does
# the two root steps and nothing else:
#
#   pos-edge-vault seal     Seals 32 random bytes as the vault key: with this machine's TPM2 and
#                           systemd's host key together when it has a usable TPM2, with the host key
#                           alone otherwise. A key that is already sealed and still unseals is kept,
#                           so running it again changes nothing. A key that no longer unseals (a
#                           cleared TPM, a disk moved from another machine) is replaced, and it and
#                           the secrets sealed under it are moved aside, since nothing can open them
#                           now; the box is then activated again. If the failure was one that passes,
#                           moving both back undoes it. A seal that fails changes nothing.
#   pos-edge-vault unseal   At every start of pos-edge, from its unit (ExecStartPre=-+). Decrypts
#                           the vault key into /run/pos-edge-vault/vault-key, which the `pos` group
#                           may read and only root may write. On a machine with no sealed key yet it
#                           seals one first: that is how a box made from a generic image gets its own
#                           key at its first boot, never one baked into the image. A key that exists
#                           and does not unseal is left alone and reported: replacing it is `seal`'s
#                           decision, made by a person, because a failure here may pass.
#
# It never reads a file the service wrote, and it renames the service's directory only as an entry
# (mv -T), so the service cannot point it anywhere else. Installed to
# /usr/local/libexec/pos-edge/pos-edge-vault, root-owned, by the installers; the copies they carry
# must match this file, which dashboard/scripts/installer-syntax.mjs checks.

set -eu

SEALED=/etc/pos-edge/credstore/vault-key.cred
NAME=pos-edge-vault-key
RUNDIR=/run/pos-edge-vault
SECRETS=/var/lib/pos-edge/vault
GROUP=pos

die() {
  echo "pos-edge-vault: $*" >&2
  exit 1
}

command -v systemd-creds >/dev/null 2>&1 ||
  die "systemd-creds is not installed; it ships with systemd 250 and later"

# One at a time: pos-edge-claim.service and pos-edge.service both run `unseal`, and two first-boot
# seals racing would leave two keys.
if command -v flock >/dev/null 2>&1; then
  exec 9>/run/pos-edge-vault.lock
  flock 9
fi

# Seals a new vault key into $SEALED.new and sets $how. Touches nothing else, so a failure leaves the
# box as it was.
seal_new_key() {
  install -d -o root -g root -m 0700 "$(dirname "$SEALED")"
  rm -f "$SEALED.new"
  # The host key. Idempotent: an existing one is kept.
  systemd-creds setup >/dev/null 2>&1 || true
  # --tpm2-pcrs= binds the key to the TPM2 chip and not to the boot measurements, so a firmware
  # update does not cost the store its activation. The threat this answers is a copied disk. A TPM2
  # that will not seal is no reason to leave the box without a key, so the host key alone follows.
  if systemd-creds has-tpm2 >/dev/null 2>&1 &&
    (umask 077 && head -c 32 /dev/urandom |
      systemd-creds encrypt --name="$NAME" --with-key=host+tpm2 --tpm2-pcrs= - "$SEALED.new"); then
    how="with this machine's TPM2 and its host key"
  elif (umask 077 && head -c 32 /dev/urandom |
    systemd-creds encrypt --name="$NAME" --with-key=host - "$SEALED.new"); then
    how="with the host key alone: this machine has no usable TPM2, so a copy of its disk can open it"
  else
    rm -f "$SEALED.new"
    die "could not seal a vault key"
  fi
}

# Puts the key seal_new_key made in place. Secrets sealed before it cannot open under it, so they
# are moved aside, with the key they were sealed under when it is still there. The copy of the old
# key in $RUNDIR goes too: `unseal` keeps a copy through a failure that passes only because it is
# the same key.
install_new_key() {
  stamp=$(date +%Y%m%d%H%M%S)
  # -T renames the entry itself and never moves into a directory, so a link the service left there
  # cannot redirect root. The service's directory goes first: if that fails, nothing has changed.
  if [ -e "$SECRETS" ] || [ -L "$SECRETS" ]; then
    mv -f -T "$SECRETS" "$SECRETS.unopenable-$stamp"
    echo "pos-edge-vault: moved the secrets sealed under the old key to" \
      "$SECRETS.unopenable-$stamp; restart pos-edge, then activate this box again" >&2
  fi
  if [ -e "$SEALED" ]; then
    mv -f -T "$SEALED" "$SEALED.unopenable-$stamp"
  fi
  rm -f "$RUNDIR/vault-key"
  mv -f "$SEALED.new" "$SEALED"
  echo "pos-edge-vault: sealed a vault key $how"
}

case "${1:-}" in
seal)
  if [ -e "$SEALED" ]; then
    if systemd-creds decrypt --name="$NAME" "$SEALED" - >/dev/null 2>&1; then
      echo "pos-edge-vault: the vault key is sealed and unseals on this machine"
      exit 0
    fi
    echo "pos-edge-vault: the sealed vault key does not unseal on this machine; sealing a new one" >&2
  fi
  seal_new_key
  install_new_key
  ;;
unseal)
  if [ ! -e "$SEALED" ]; then
    seal_new_key
    install_new_key
  fi
  install -d -o root -g "$GROUP" -m 0750 "$RUNDIR"
  if ! (umask 027 && systemd-creds decrypt --name="$NAME" "$SEALED" "$RUNDIR/vault-key.new"); then
    # A copy unsealed earlier in this boot is of this same key, since install_new_key removes any
    # other, so it stays: a failure that passes costs nothing. With no copy the store trades
    # without cloud sync until this is fixed.
    rm -f "$RUNDIR/vault-key.new"
    die "the vault key did not unseal on this start; if it keeps failing, run" \
      "'pos-edge-vault seal', restart pos-edge and activate this box again"
  fi
  chgrp "$GROUP" "$RUNDIR/vault-key.new"
  chmod 0440 "$RUNDIR/vault-key.new"
  mv -f "$RUNDIR/vault-key.new" "$RUNDIR/vault-key"
  ;;
*)
  die "usage: pos-edge-vault seal | unseal"
  ;;
esac
POS_EDGE_VAULT
}

vault_dropin() {
  cat <<'POS_EDGE_VAULT_CONF'
# Sealed secrets for a Linux store box (ADR-0151). Installed as
# /etc/systemd/system/pos-edge.service.d/vault.conf, and on an appliance also as
# pos-edge-claim.service.d/vault.conf, so the credential a first-boot claim collects is sealed too.
# Without it the edge keeps its device credential in the kernel keyring, which a reboot empties. The
# copies the installers carry must match this file (dashboard/scripts/installer-syntax.mjs).
[Service]
# "+" runs the helper as root, because systemd-creds is root-only; the service itself stays `pos`.
# "-" lets the edge start if the key does not unseal: its vault then answers errors, so the counter
# keeps trading and cloud sync waits, and the log says what to run.
ExecStartPre=-+/usr/local/libexec/pos-edge/pos-edge-vault unseal
Environment=POS_EDGE_VAULT_KEY=/run/pos-edge-vault/vault-key
Environment=POS_EDGE_VAULT_DIR=/var/lib/pos-edge/vault
POS_EDGE_VAULT_CONF
}

# Sealed secrets (ADR-0151): the root helper, and the drop-in that has it unseal the vault key before
# the edge starts and before a first-boot claim, which is what collects the credential. On a running
# system the key is sealed now, and the drop-ins go in only once it is: an edge whose drop-in cannot
# get it a key answers vault errors instead of using the keyring. While an image is built in a
# chroot nothing is sealed, since that would give every box made from the image the same host key,
# or bind the key to the build machine's TPM2: the drop-ins go in, and each box seals its own key at
# its first start.
install_vault() {
  local helper dropin unit
  if ! command -v systemd-creds >/dev/null 2>&1; then
    warn "no systemd-creds here (systemd 250 or later has it): the device credential stays in the kernel keyring, and a reboot means activating again"
    return 0
  fi
  run install -d -o root -g root -m 0755 "$(dirname "$VAULT_HELPER")"
  helper="$(script_dir)/../edge/pos-edge-vault"
  if [ -f "$helper" ]; then
    say "installing pos-edge-vault from this checkout (deploy/edge/pos-edge-vault)"
    run install -o root -g root -m 0755 "$helper" "$VAULT_HELPER"
  else
    say "installing pos-edge-vault (the copy of deploy/edge/pos-edge-vault in this script)"
    vault_helper | write_file "$VAULT_HELPER" 0755 root:root quiet
  fi
  if systemd_running; then
    if ! run "$VAULT_HELPER" seal; then
      warn "no vault key could be sealed: the device credential stays in the kernel keyring, and a reboot means activating again"
      for unit in pos-edge pos-edge-claim; do
        run rm -f "$UNITS/$unit.service.d/vault.conf"
      done
      return 0
    fi
  else
    say "not sealing a vault key while building an image: each box seals its own at its first boot"
  fi
  dropin="$(script_dir)/../edge/pos-edge.service.d/vault.conf"
  for unit in pos-edge pos-edge-claim; do
    run install -d -o root -g root -m 0755 "$UNITS/$unit.service.d"
    if [ -f "$dropin" ]; then
      run install -o root -g root -m 0644 "$dropin" "$UNITS/$unit.service.d/vault.conf"
    else
      vault_dropin | write_file "$UNITS/$unit.service.d/vault.conf" 0644 root:root quiet
    fi
  done
}

# The env file the unit reads (EnvironmentFile=-/etc/pos-edge/env): written once, every line a
# comment, so that whoever opens it finds what goes there. The console's installer writes the same
# file with the store key in it; this script is never handed one.
env_template() {
  cat <<'POS_EDGE_ENV'
# /etc/pos-edge/env — the store's secrets, read by pos-edge.service (EnvironmentFile=).
# Root-owned, mode 0600, never committed. Written by deploy/appliance/provision.sh with every line
# commented out, and never overwritten by it afterwards.
#
# No store key is needed: the box syncs with the device credential its activation or claim mints,
# sealed under the vault key where one is (ADR-0151). To use a store key (read_config +
# relay_orders) instead, uncomment this line and paste the key after the =.
# POS_EDGE_SYNC_KEY=
#
# Where the store publishes its committed events: tls://:<token>@<cloud host>:4222, where the token
# is the fleet's, read on the cloud box with: sudo sed -n 's/  token: //p' deploy/secrets/nats.conf
# POS_EDGE_NATS_URL=
POS_EDGE_ENV
}

install_env_file() {
  run install -d -o root -g root -m 0755 "$ETC"
  if [ -e "$ETC/env" ]; then
    say "$ETC/env exists: leaving it alone"
  else
    env_template | write_file "$ETC/env" 0600 root:root
  fi
}

# config.toml exactly as the console writes it for a store with no name, no custom port and no
# store_path (configToml in dashboard/src/installers.mjs), with the two placeholders filled in by
# config_toml. installer-syntax.mjs renders both and fails the dashboard build if they differ.
config_template() {
  cat <<'POS_EDGE_CONFIG'
# pos_edge bootstrap configuration
# Store:  @STORE_ID@
#
# This file tells the store server WHICH store it is and WHICH cloud to dial. It carries no
# credential — that lives in the environment file (or the OS keyring), never here.
# Save it as config.toml beside the pos_edge binary, or point POS_EDGE_CONFIG at its path.

store_id = "@STORE_ID@"
cloud_url = "@CLOUD_URL@"

# Optional — override the listen address (default 0.0.0.0:8787):
# bind = "0.0.0.0:8787"

# Optional — the LAN IP to advertise in the pairing QR; pin it with a DHCP reservation:
# advertised_ip = "192.168.1.50"

# Optional — where the SQLite event store lives (default store.sqlite):
# store_path = "store.sqlite"

# Where this store publishes its committed events. Both values are the whole fleet's, not this
# store's, and they must match the [nats] section of cloud.toml on the cloud box — which is why
# they are generated rather than typed. The server URL is NOT here: it carries the broker token,
# so it lives in the env file below.
#
# Keep this table LAST. Everything above it is a top-level key, and a commented line moved below
# this header would be read as part of [nats] and refused at load.
[nats]
stream = "POS_FLEET"
subject = "pos.fleet.events"
POS_EDGE_CONFIG
}

# config_toml <store ULID> <cloud origin>
config_toml() {
  local text
  text=$(config_template)
  text=${text//@STORE_ID@/"$1"}
  text=${text//@CLOUD_URL@/"$2"}
  printf '%s\n' "$text"
}

# The first-boot unit of a box installed without a store (ADR-0148). `claim` is the edge's own
# subcommand; this unit only decides when it runs, as whom, and what happens after.
claim_unit_template() {
  cat <<'POS_EDGE_CLAIM'
# pos-edge-claim.service — the first boot of a generic store box (ADR-0148, ADR-0150).
#
# Installed by deploy/appliance/provision.sh when it is given no --store, and inert once the box has
# a config.toml. `pos-edge claim` asks the cloud for a code and shows it; a person with console
# rights claims the box for a store (Activation, Claim a box); claim writes config.toml and keeps the
# device credential in the keyring, then exits. This unit then starts the edge, and on every later
# boot the condition below skips it.
#
# User=pos, not root, deliberately: the Linux keyring the edge keeps its credential in is per user
# (ADR-0086), so a credential collected as root is one pos-edge.service, which runs as pos, never
# finds.

[Unit]
Description=Claim this store box from the console (first boot)
Documentation=https://github.com/buitrungtuan-4ps/pos-framework-4ps/blob/main/docs/guides/appliance.md
ConditionPathExists=!/var/lib/pos-edge/config.toml
After=network-online.target
Wants=network-online.target
Before=pos-edge.service

[Service]
Type=oneshot
User=pos
Group=pos
StateDirectory=pos-edge
WorkingDirectory=/var/lib/pos-edge
# Where claim writes config.toml: the path pos-edge.service reads.
Environment=POS_EDGE_CONFIG=/var/lib/pos-edge/config.toml
Environment=RUST_LOG=info
ExecStart=/var/lib/pos-edge/bin/current claim --cloud @CLOUD_URL@
# A claim that exits without writing config.toml has not claimed anything: fail, and try again.
ExecStartPost=/usr/bin/test -f /var/lib/pos-edge/config.toml
# '+' runs this one as root and outside the sandbox below: the pos user may not enable units.
# --no-block, because pos-edge.service is ordered after this unit, and waiting for its start from
# inside this unit's own start would wait forever.
ExecStartPost=+/usr/bin/systemctl enable --now --no-block pos-edge.service
# A person is in the loop, so no start timeout. A failed attempt (no network yet, say) is retried.
TimeoutStartSec=infinity
Restart=on-failure
RestartSec=30

# The sandbox pos-edge.service runs in: its state directory writable, everything else read-only.
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
POS_EDGE_CLAIM
}

claim_unit() {
  local text
  text=$(claim_unit_template)
  printf '%s\n' "${text//@CLOUD_URL@/"$1"}"
}

install_identity() {
  if [ -f "$CONFIG" ]; then
    say "config.toml exists: this box is store $(config_value store_id) on $(config_value cloud_url); leaving it alone"
  elif [ -n "$STORE" ]; then
    say "writing config.toml for store $STORE"
    config_toml "$STORE" "$CLOUD" | write_file "$CONFIG" 0644 "$SERVICE_USER:$SERVICE_USER"
  else
    say "no --store: installing pos-edge-claim.service, so the box claims itself on first boot"
    claim_unit "$CLOUD" | write_file "$UNITS/pos-edge-claim.service" 0644 root:root
  fi
}

kiosk_unit() {
  cat <<'POS_KIOSK_UNIT'
# pos-kiosk.service — the till, full screen on this machine's own display (ADR-0150).
#
# Installed by deploy/appliance/provision.sh --kiosk. There is no display manager: this unit logs the
# unprivileged pos-kiosk user in on tty1 through PAM (/etc/pam.d/pos-kiosk). That registers a logind
# session on the seat, and the session is what gives cage the screen, keyboard and touch without any
# group membership. /usr/local/libexec/pos-kiosk then waits for something to show and runs cage with
# Chromium on it.

[Unit]
Description=POS till kiosk on tty1 (cage and Chromium)
Documentation=https://github.com/buitrungtuan-4ps/pos-framework-4ps/blob/main/docs/guides/appliance.md
After=systemd-user-sessions.service plymouth-quit-wait.service
Wants=dbus.socket systemd-logind.service
After=dbus.socket systemd-logind.service
# Ordered after the edge, never tied to it: on an unclaimed box the edge is not running yet, and the
# kiosk shows the claim code instead.
After=pos-edge.service
# tty1 belongs to the kiosk; a login prompt started there is replaced. provision.sh also disables it.
Conflicts=getty@tty1.service
After=getty@tty1.service
ConditionPathExists=/dev/tty0

[Service]
Type=simple
User=pos-kiosk
PAMName=pos-kiosk
TTYPath=/dev/tty1
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes
StandardInput=tty-fail
StandardOutput=journal
StandardError=journal
UtmpIdentifier=tty1
UtmpMode=user
Environment=XDG_SESSION_TYPE=wayland
ExecStart=/usr/local/libexec/pos-kiosk
# Chromium closing, cage exiting, the claim completing: all of them come back here.
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
POS_KIOSK_UNIT
}

kiosk_pam() {
  cat <<'POS_KIOSK_PAM'
# /etc/pam.d/pos-kiosk — the session pos-kiosk.service opens on tty1. Installed by
# deploy/appliance/provision.sh --kiosk.
#
# systemd never calls pam_authenticate for a unit's PAMName=, so nothing here asks for a password,
# and the account has none that could be typed. What matters is the last line: pam_systemd registers
# the session with logind on the seat, and logind is what grants cage the display and input devices.
auth     required  pam_unix.so nullok
account  required  pam_unix.so
session  required  pam_unix.so
session  required  pam_systemd.so
POS_KIOSK_PAM
}

kiosk_launcher() {
  cat <<'POS_KIOSK_LAUNCHER'
#!/bin/sh
# Copyright (c) 2026 Pizza 4P's. All rights reserved.
# Proprietary and confidential. Internal use only. See LICENSE.
#
# pos-kiosk — what pos-kiosk.service runs on tty1: cage, a single-window Wayland compositor, with
# Chromium full screen in it. Installed by deploy/appliance/provision.sh --kiosk (ADR-0150), which is
# the place to change it.
#
# It waits until there is something to show, so the screen stays blank rather than showing an error:
#   - a box with a config.toml is a store, and shows the till once the edge answers /healthz;
#   - a box without one is being claimed, and shows the page `pos-edge claim` serves the code on
#     (ADR-0148). When the claim writes config.toml the session ends, and the unit's Restart=always
#     brings it back on the till.
# What it is waiting for goes to the journal: journalctl -u pos-kiosk
set -eu

config=/var/lib/pos-edge/config.toml
claim_page=http://127.0.0.1:8080/

if [ -f "$config" ]; then
  # Wherever the edge binds: config.toml's `bind`, or its default. A wildcard bind is reached on
  # loopback, which a browser treats as a secure context even over plain http.
  bind=$(sed -n 's/^[[:space:]]*bind[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$config" | tail -n 1)
  bind=${bind:-0.0.0.0:8787}
  host=${bind%:*}
  case $host in
    0.0.0.0 | "[::]") host=127.0.0.1 ;;
  esac
  url="http://$host:${bind##*:}/"
  probe="${url}healthz"
else
  url=$claim_page
  probe=$claim_page
fi

said=""
while :; do
  browser=""
  for candidate in /usr/bin/chromium /snap/bin/chromium; do
    if [ -x "$candidate" ]; then
      browser=$candidate
      break
    fi
  done
  if [ -z "$browser" ]; then
    message="no Chromium at /usr/bin/chromium or /snap/bin/chromium (Debian: apt-get install chromium; Ubuntu: snap install chromium)"
  elif curl -fs -o /dev/null --max-time 2 "$probe"; then
    break
  else
    message="waiting for $probe"
  fi
  if [ "$message" != "$said" ]; then
    echo "$message" >&2
    said=$message
  fi
  sleep 2
done

echo "opening $url in $browser" >&2
# No -s, deliberately: without it cage refuses to switch consoles, so Ctrl+Alt+F-keys do not reach a
# login at the counter. --ozone-platform=wayland draws on cage directly rather than through Xwayland.
set -- cage -- "$browser" --kiosk --noerrdialogs --disable-infobars --no-first-run \
  --ozone-platform=wayland --app="$url"
if [ -f "$config" ]; then
  exec "$@"
fi

# The claim page: run the session, and end it once the box has been claimed.
"$@" <&0 &
session=$!
while kill -0 "$session" 2>/dev/null; do
  if [ -f "$config" ]; then
    kill "$session" 2>/dev/null || true
    break
  fi
  sleep 5
done
wait "$session" || true
POS_KIOSK_LAUNCHER
}

chromium_deb_available() {
  local candidate
  candidate=$(apt-cache policy chromium 2>/dev/null | sed -n 's/^ *Candidate: //p')
  [ -n "$candidate" ] && [ "$candidate" != "(none)" ]
}

# Debian ships Chromium as a deb. Ubuntu 24.04 ships it only as a snap (its chromium-browser deb is
# a stub that installs the snap), and snapd needs a running system, so an image built in a chroot
# gets it on first boot instead. The launcher looks in both places and waits until one exists.
# While neither does, every run refreshes the package lists to look for a deb again.
install_chromium() {
  if [ -x /usr/bin/chromium ] || [ -x /snap/bin/chromium ]; then
    say "Chromium is installed"
    return 0
  fi
  apt_update
  if chromium_deb_available; then
    apt_install chromium
  elif command -v snap >/dev/null 2>&1 && [ -S /run/snapd.socket ]; then
    say "no chromium deb in this distribution's archive: installing the Chromium snap"
    run snap install chromium
  else
    warn "no chromium deb here and no running snapd (a chroot or an image build?). On Ubuntu 24.04 Chromium is a snap: run 'snap install chromium' once the box is up. The kiosk waits for it."
  fi
}

install_kiosk() {
  say "installing the kiosk: cage and Chromium on tty1"
  apt_install cage curl dbus libpam-systemd
  install_chromium
  if id -u "$KIOSK_USER" >/dev/null 2>&1; then
    say "kiosk account '$KIOSK_USER' exists"
  else
    # A home under /home, because Chromium keeps a profile and the snap refuses any other place.
    run useradd --create-home --shell /usr/sbin/nologin "$KIOSK_USER"
  fi
  kiosk_pam | write_file "$KIOSK_PAM" 0644 root:root
  run install -d -o root -g root -m 0755 "$(dirname "$KIOSK_LAUNCHER")"
  kiosk_launcher | write_file "$KIOSK_LAUNCHER" 0755 root:root
  kiosk_unit | write_file "$UNITS/pos-kiosk.service" 0644 root:root
}

activate_units() {
  local booted=false
  if systemd_running; then
    booted=true
    run systemctl daemon-reload
  fi
  if [ -f "$CONFIG" ] || [ -n "$STORE" ]; then
    if [ "$booted" = true ]; then
      # A restart, not `enable --now`: on a box already running, it applies the unit and the vault
      # drop-in, and moves a credential still in the kernel keyring into the sealed store before the
      # next reboot can empty the keyring (ADR-0151).
      run systemctl enable pos-edge.service
      run systemctl restart pos-edge.service
    else
      run systemctl enable pos-edge.service
    fi
  else
    run systemctl enable pos-edge-claim.service
    if [ "$booted" = true ]; then
      run systemctl start --no-block pos-edge-claim.service
    fi
  fi
  if [ "$KIOSK" = true ]; then
    # Enabled, never started here: starting it replaces the login on tty1, which may be the very
    # session running this script. It takes the screen at the next boot.
    run systemctl disable getty@tty1.service
    run systemctl enable pos-kiosk.service
  fi
}

# The tills reach the edge on its port, and a stock Debian or Ubuntu box filters nothing. So this acts
# only on a filter that is running, exactly as the console's installer does: a blind `ufw allow` on a
# box where ufw is installed but inactive records a rule that takes effect, unannounced, the day
# somebody enables the firewall for another reason.
open_port() {
  local port=$1 state
  if command -v ufw >/dev/null 2>&1; then
    state=$(ufw status 2>/dev/null || true)
    if [[ $state == "Status: active"* ]]; then
      run ufw allow "$port/tcp" ||
        warn "ufw could not allow $port/tcp: open it by hand, or the tills will time out"
      return 0
    fi
  fi
  if command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then
    { run firewall-cmd --permanent --add-port="$port/tcp" && run firewall-cmd --reload; } ||
      warn "firewalld could not allow $port/tcp: open it by hand, or the tills will time out"
    return 0
  fi
  say "no active ufw or firewalld: assuming nothing filters $port/tcp on this box"
}

summary() {
  echo
  if [ "$DRY_RUN" = true ]; then
    echo "Dry run: nothing was changed."
    return 0
  fi
  if [ -f "$CONFIG" ]; then
    echo "pos_edge is installed for store $(config_value store_id)."
    if systemd_running; then
      systemctl --no-pager --lines=0 status pos-edge.service || true
    fi
    echo "Next, on a box that is not activated yet: issue a code in the console (Activation) and type it"
    echo "at http://<this box>:$(edge_port)/setup, then pair the tills (docs/guides/bring-a-store-online.md)."
  else
    echo "This box claims itself (ADR-0148). pos-edge-claim.service shows a code (on this screen with"
    echo "--kiosk); enter it in the console under Activation, Claim a box, and the edge starts once the"
    echo "box is claimed. It needs a pos-edge that has the 'claim' command. Follow it with:"
    echo "  journalctl -u pos-edge-claim -f"
  fi
  if [ "$KIOSK" = true ]; then
    echo "The kiosk takes tty1 from the next boot: reboot, or run 'systemctl start pos-kiosk' from an"
    echo "SSH session (not from tty1, whose login it replaces)."
  fi
  if ! systemd_running; then
    echo "systemd is not running here (a chroot or an image build), so nothing was started: the units"
    echo "are enabled and start on the first boot."
  fi
}

main() {
  parse_args "$@"
  validate_args
  preflight
  if [ "$DRY_RUN" = true ]; then
    say "DRY RUN on $OS: nothing below is executed or written; '+' lines are what a real run does"
  else
    say "provisioning a store box on $OS"
  fi
  install_service_user
  install_binary
  apt_install fonts-dejavu-core
  install_edge_unit
  install_vault
  install_env_file
  install_identity
  if [ "$KIOSK" = true ]; then
    install_kiosk
  fi
  activate_units
  open_port "$(edge_port)"
  summary
}

# Run, not sourced: a test can source this file for its functions without provisioning anything.
if [ "${BASH_SOURCE[0]:-$0}" = "$0" ]; then
  main "$@"
fi
