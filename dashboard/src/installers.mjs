// The four artifacts the new-store wizard hands a technician: `config.toml`, the `env` file, and one
// installer per store OS (roadmap-v3 **R3** for Linux, **R4** for Windows).
//
// # Why this is a `.mjs` and not a `.tsx` helper beside the screen
//
// Because it has a second caller that is not a browser: `scripts/installer-syntax.mjs` renders every
// artifact with representative values and puts the result through a real parser — `sh -n` for the
// shell script, PowerShell's own parser for the `.ps1` on the Windows CI runner. Plain ESM with no
// imports is what lets bare `node` run it on a runner with no toolchain set up, and Vite bundles it
// for the screen unchanged. `installers.d.mts` gives the TypeScript side its types.
//
// That gate is the whole reason this file exists. These scripts are typed by nobody and run as root
// on a shop's only till: a stray quote in a generated heredoc is a store that does not open. Until
// now the generated shell script was checked by nothing at all — `sh -n` appears nowhere in the tree
// — and the Windows script did not exist, which [issue #182](https://github.com/buitrungtuan-4ps/pos-framework-4ps/issues/182)
// attributed to the absence of a way to check one.

/** The bind port the edge defaults to when `config.toml` names none (`pos_edge`'s `DEFAULT_BIND`). */
export const DEFAULT_BIND_PORT = "8787";

/**
 * `deploy/edge/pos-edge-vault`, verbatim, without its last newline: the root helper that seals and
 * unseals a Linux box's vault key ([ADR-0151](../../docs/adr/0151-a-headless-linux-box-seals-its-secrets-with-systemd-creds.md)).
 * The generated installer writes it to `/usr/local/libexec/pos-edge`. The file in the repository is
 * the source of truth; `scripts/installer-syntax.mjs` fails the build when this copy stops matching it.
 */
export const VAULT_HELPER = [
  "#!/bin/sh",
  "# Copyright (c) 2026 Pizza 4P's. All rights reserved.",
  "# Proprietary and confidential. Internal use only. See LICENSE.",
  "#",
  "# pos-edge-vault — the root half of a Linux store box's sealed secrets (ADR-0151).",
  "#",
  "# The store runs as the unprivileged `pos` user and keeps its device credential sealed under a",
  "# vault key. Sealing that key needs systemd-creds, and systemd-creds needs root, so this script does",
  "# the two root steps and nothing else:",
  "#",
  "#   pos-edge-vault seal     Seals 32 random bytes as the vault key: with this machine's TPM2 and",
  "#                           systemd's host key together when it has a usable TPM2, with the host key",
  "#                           alone otherwise. A key that is already sealed and still unseals is kept,",
  "#                           so running it again changes nothing. A key that no longer unseals (a",
  "#                           cleared TPM, a disk moved from another machine) is replaced, and it and",
  "#                           the secrets sealed under it are moved aside, since nothing can open them",
  "#                           now; the box is then activated again. If the failure was one that passes,",
  "#                           moving both back undoes it. A seal that fails changes nothing.",
  "#   pos-edge-vault unseal   At every start of pos-edge, from its unit (ExecStartPre=-+). Decrypts",
  "#                           the vault key into /run/pos-edge-vault/vault-key, which the `pos` group",
  "#                           may read and only root may write. On a machine with no sealed key yet it",
  "#                           seals one first: that is how a box made from a generic image gets its own",
  "#                           key at its first boot, never one baked into the image. A key that exists",
  "#                           and does not unseal is left alone and reported: replacing it is `seal`'s",
  "#                           decision, made by a person, because a failure here may pass.",
  "#",
  "# It never reads a file the service wrote, and it renames the service's directory only as an entry",
  "# (mv -T), so the service cannot point it anywhere else. Installed to",
  "# /usr/local/libexec/pos-edge/pos-edge-vault, root-owned, by the installers; the copies they carry",
  "# must match this file, which dashboard/scripts/installer-syntax.mjs checks.",
  "",
  "set -eu",
  "",
  "SEALED=/etc/pos-edge/credstore/vault-key.cred",
  "NAME=pos-edge-vault-key",
  "RUNDIR=/run/pos-edge-vault",
  "SECRETS=/var/lib/pos-edge/vault",
  "GROUP=pos",
  "",
  "die() {",
  "  echo \"pos-edge-vault: $*\" >&2",
  "  exit 1",
  "}",
  "",
  "command -v systemd-creds >/dev/null 2>&1 ||",
  "  die \"systemd-creds is not installed; it ships with systemd 250 and later\"",
  "",
  "# One at a time: pos-edge-claim.service and pos-edge.service both run `unseal`, and two first-boot",
  "# seals racing would leave two keys.",
  "if command -v flock >/dev/null 2>&1; then",
  "  exec 9>/run/pos-edge-vault.lock",
  "  flock 9",
  "fi",
  "",
  "# Seals a new vault key into $SEALED.new and sets $how. Touches nothing else, so a failure leaves the",
  "# box as it was.",
  "seal_new_key() {",
  "  install -d -o root -g root -m 0700 \"$(dirname \"$SEALED\")\"",
  "  rm -f \"$SEALED.new\"",
  "  # The host key. Idempotent: an existing one is kept.",
  "  systemd-creds setup >/dev/null 2>&1 || true",
  "  # --tpm2-pcrs= binds the key to the TPM2 chip and not to the boot measurements, so a firmware",
  "  # update does not cost the store its activation. The threat this answers is a copied disk. A TPM2",
  "  # that will not seal is no reason to leave the box without a key, so the host key alone follows.",
  "  if systemd-creds has-tpm2 >/dev/null 2>&1 &&",
  "    (umask 077 && head -c 32 /dev/urandom |",
  "      systemd-creds encrypt --name=\"$NAME\" --with-key=host+tpm2 --tpm2-pcrs= - \"$SEALED.new\"); then",
  "    how=\"with this machine's TPM2 and its host key\"",
  "  elif (umask 077 && head -c 32 /dev/urandom |",
  "    systemd-creds encrypt --name=\"$NAME\" --with-key=host - \"$SEALED.new\"); then",
  "    how=\"with the host key alone: this machine has no usable TPM2, so a copy of its disk can open it\"",
  "  else",
  "    rm -f \"$SEALED.new\"",
  "    die \"could not seal a vault key\"",
  "  fi",
  "}",
  "",
  "# Puts the key seal_new_key made in place. Secrets sealed before it cannot open under it, so they",
  "# are moved aside, with the key they were sealed under when it is still there. The copy of the old",
  "# key in $RUNDIR goes too: `unseal` keeps a copy through a failure that passes only because it is",
  "# the same key.",
  "install_new_key() {",
  "  stamp=$(date +%Y%m%d%H%M%S)",
  "  # -T renames the entry itself and never moves into a directory, so a link the service left there",
  "  # cannot redirect root. The service's directory goes first: if that fails, nothing has changed.",
  "  if [ -e \"$SECRETS\" ] || [ -L \"$SECRETS\" ]; then",
  "    mv -f -T \"$SECRETS\" \"$SECRETS.unopenable-$stamp\"",
  "    echo \"pos-edge-vault: moved the secrets sealed under the old key to\" \\",
  "      \"$SECRETS.unopenable-$stamp; restart pos-edge, then activate this box again\" >&2",
  "  fi",
  "  if [ -e \"$SEALED\" ]; then",
  "    mv -f -T \"$SEALED\" \"$SEALED.unopenable-$stamp\"",
  "  fi",
  "  rm -f \"$RUNDIR/vault-key\"",
  "  mv -f \"$SEALED.new\" \"$SEALED\"",
  "  echo \"pos-edge-vault: sealed a vault key $how\"",
  "}",
  "",
  "case \"${1:-}\" in",
  "seal)",
  "  if [ -e \"$SEALED\" ]; then",
  "    if systemd-creds decrypt --name=\"$NAME\" \"$SEALED\" - >/dev/null 2>&1; then",
  "      echo \"pos-edge-vault: the vault key is sealed and unseals on this machine\"",
  "      exit 0",
  "    fi",
  "    echo \"pos-edge-vault: the sealed vault key does not unseal on this machine; sealing a new one\" >&2",
  "  fi",
  "  seal_new_key",
  "  install_new_key",
  "  ;;",
  "unseal)",
  "  if [ ! -e \"$SEALED\" ]; then",
  "    seal_new_key",
  "    install_new_key",
  "  fi",
  "  install -d -o root -g \"$GROUP\" -m 0750 \"$RUNDIR\"",
  "  if ! (umask 027 && systemd-creds decrypt --name=\"$NAME\" \"$SEALED\" \"$RUNDIR/vault-key.new\"); then",
  "    # A copy unsealed earlier in this boot is of this same key, since install_new_key removes any",
  "    # other, so it stays: a failure that passes costs nothing. With no copy the store trades",
  "    # without cloud sync until this is fixed.",
  "    rm -f \"$RUNDIR/vault-key.new\"",
  "    die \"the vault key did not unseal on this start; if it keeps failing, run\" \\",
  "      \"'pos-edge-vault seal', restart pos-edge and activate this box again\"",
  "  fi",
  "  chgrp \"$GROUP\" \"$RUNDIR/vault-key.new\"",
  "  chmod 0440 \"$RUNDIR/vault-key.new\"",
  "  mv -f \"$RUNDIR/vault-key.new\" \"$RUNDIR/vault-key\"",
  "  ;;",
  "*)",
  "  die \"usage: pos-edge-vault seal | unseal\"",
  "  ;;",
  "esac",
].join("\n");

/** `deploy/edge/pos-edge.service.d/vault.conf`, verbatim, without its last newline, likewise checked. */
export const VAULT_DROPIN = [
  "# Sealed secrets for a Linux store box (ADR-0151). Installed as",
  "# /etc/systemd/system/pos-edge.service.d/vault.conf, and on an appliance also as",
  "# pos-edge-claim.service.d/vault.conf, so the credential a first-boot claim collects is sealed too.",
  "# Without it the edge keeps its device credential in the kernel keyring, which a reboot empties. The",
  "# copies the installers carry must match this file (dashboard/scripts/installer-syntax.mjs).",
  "[Service]",
  "# \"+\" runs the helper as root, because systemd-creds is root-only; the service itself stays `pos`.",
  "# \"-\" lets the edge start if the key does not unseal: its vault then answers errors, so the counter",
  "# keeps trading and cloud sync waits, and the log says what to run.",
  "ExecStartPre=-+/usr/local/libexec/pos-edge/pos-edge-vault unseal",
  "Environment=POS_EDGE_VAULT_KEY=/run/pos-edge-vault/vault-key",
  "Environment=POS_EDGE_VAULT_DIR=/var/lib/pos-edge/vault",
].join("\n");

/**
 * The JetStream stream and subject every store publishes its committed events into
 * ([ADR-0087](../../docs/adr/0087-edge-relay-and-event-publish.md) Amendment 1). **Fleet-wide, and
 * identical on every box** — `pos_cloud` binds one durable consumer to one named stream, so a
 * per-store stream would be ingested one store deep, and a per-store subject inside a shared stream
 * would be captured for the first box to connect and refused for every one after it (the edge's
 * handshake is a create-or-get, which does not add a subject to a stream that already exists). They
 * must match `cloud.toml`'s `[nats] stream` and `filter_subject` on the cloud box, which
 * `bootstrap.sh` documents with these same two values.
 */
export const FLEET_STREAM = "POS_FLEET";
/** The subject half of the pair above. */
export const FLEET_SUBJECT = "pos.fleet.events";

/**
 * The client port the broker publishes under `TLS_MODE`'s certificate-bearing postures
 * ([ADR-0089](../../docs/adr/0089-edge-event-bus-transport.md)).
 */
export const NATS_CLIENT_PORT = "4222";

/**
 * Where a Windows box keeps its state, and the reason the Windows config carries an absolute
 * `store_path` where the Linux one does not.
 *
 * `store_path` is handed to SQLite as written, so a relative one resolves against the **process
 * working directory**. The systemd unit sets `WorkingDirectory=/var/lib/pos-edge`, so the default
 * `store.sqlite` lands where it should. The Service Control Manager sets no such thing: it starts a
 * service in `C:\Windows\System32`. A Windows store installed by hand from the README, with the
 * wizard's config as generated, therefore put its database — and with it `bin\`, since the update
 * slot directory is derived from the database's parent — under `System32`. Writing the path out is
 * the fix, and it is exactly the class of mistake a generated installer exists to remove.
 */
export const WINDOWS_ROOT = "C:\\ProgramData\\pos-edge";

/**
 * Escapes a value for a `sh` double-quoted string.
 *
 * A store is named by a person in a form, so it can hold a quote, a backslash or a `$`. Unescaped,
 * any of the three turns the generated installer into a syntax error or — worse — a command
 * substitution running as root. Nothing checked that before `scripts/installer-syntax.mjs`, which is
 * why it is worth fixing at the same time as adding the check.
 *
 * @param {string} value
 * @returns {string}
 */
function shDouble(value) {
  return value.replace(/([\\"$`])/gu, "\\$1");
}

/**
 * Escapes a value for a PowerShell single-quoted string, where doubling is the only escape.
 *
 * @param {string} value
 * @returns {string}
 */
/**
 * The byte-order mark every generated PowerShell script starts with.
 *
 * # Why a .ps1 needs one, when nothing else here does
 *
 * Windows PowerShell 5.1 — the edition that ships with Windows and the one a technician gets by
 * typing `powershell` — reads a script **without** a BOM as ANSI in the machine's code page, not as
 * UTF-8. PowerShell 7 (`pwsh`) reads it as UTF-8 either way, which is what made this a shipped
 * defect rather than a caught one: CI parsed the scripts with `pwsh` and they were fine.
 *
 * What that costs, concretely. An em dash is `E2 80 94` in UTF-8; read as CP1252 (or CP1258, on a
 * Vietnamese install) those three bytes become `â`, `€`, and **`”` — U+201D, which PowerShell's
 * parser accepts as a double-quote string delimiter.** So one em dash inside a single-quoted
 * `Write-Host` opens a string that swallows the rest of the line, and the script dies at parse time
 * with *"The string is missing the terminator: '."* — before a single line of it has run.
 *
 * A store's own name is why this cannot be fixed by writing ASCII prose instead. The wizard bakes
 * the name into the script's help block, and a Vietnamese store name is non-ASCII by nature: `Bến
 * Thành` mangles exactly the same way. The mark is the fix, and it belongs here rather than at the
 * download so that every consumer — the wizard's Blob, the checked-in template, and the syntax gate
 * — gets it without having to remember.
 *
 * Note which files do *not* get one: `config.toml`, `printAgentToml` and `env`. The edge's TOML
 * parser reads a leading BOM as part of the first key and refuses the file, and `#!/bin/sh` stops
 * being a shebang if three bytes precede it.
 */
const PS_BOM = "\ufeff";

function psSingle(value) {
  return value.replace(/'/gu, "''");
}

/**
 * The bootstrap configuration: which store this is and which cloud to dial. No credential in it.
 *
 * @param {import("./installers.d.mts").InstallerValues} v
 * @returns {string}
 */
export function configToml(v) {
  const port = v.bindPort.trim();
  return [
    "# pos_edge bootstrap configuration",
    // The parameterised template has no name to print — the store id is all it is given.
    v.storeName ? `# Store:  ${v.storeName}  (${v.storeId})` : `# Store:  ${v.storeId}`,
    ...(v.tenantLabel ? [`# Tenant: ${v.tenantLabel}  (${v.tenantId})`] : []),
    "#",
    "# This file tells the store server WHICH store it is and WHICH cloud to dial. It carries no",
    "# credential — that lives in the environment file (or the OS keyring), never here.",
    "# Save it as config.toml beside the pos_edge binary, or point POS_EDGE_CONFIG at its path.",
    "",
    `store_id = "${v.storeId}"`,
    `cloud_url = "${v.cloudUrl}"`,
    ...(port && port !== DEFAULT_BIND_PORT
      ? ["", `bind = "0.0.0.0:${port}"`]
      : [
          "",
          `# Optional — override the listen address (default 0.0.0.0:${DEFAULT_BIND_PORT}):`,
          `# bind = "0.0.0.0:${DEFAULT_BIND_PORT}"`,
        ]),
    "",
    "# Optional — the LAN IP to advertise in the pairing QR; pin it with a DHCP reservation:",
    '# advertised_ip = "192.168.1.50"',
    "",
    // The one branch: a relative default is right on Linux, where the unit sets a working directory,
    // and wrong on Windows, where SCM does not. See WINDOWS_ROOT.
    ...(v.storePath
      ? [
          "# Absolute on purpose. The Service Control Manager starts a service in C:\\Windows\\System32,",
          "# and a relative store_path is resolved against the working directory — so the default",
          "# `store.sqlite` would put this store's database, and the bin\\ update slots beside it, under",
          "# System32. The systemd unit sets WorkingDirectory= and has no such problem.",
          `store_path = "${v.storePath.replace(/\\/gu, "\\\\")}"`,
        ]
      : [
          "# Optional — where the SQLite event store lives (default store.sqlite):",
          '# store_path = "store.sqlite"',
        ]),
    "",
    "# Where this store publishes its committed events. Both values are the whole fleet's, not this",
    "# store's, and they must match the [nats] section of cloud.toml on the cloud box — which is why",
    "# they are generated rather than typed. The server URL is NOT here: it carries the broker token,",
    "# so it lives in the env file below.",
    "#",
    "# Keep this table LAST. Everything above it is a top-level key, and a commented line moved below",
    "# this header would be read as part of [nats] and refused at load.",
    "[nats]",
    `stream = "${FLEET_STREAM}"`,
    `subject = "${FLEET_SUBJECT}"`,
    "",
  ].join("\n");
}

/**
 * The box's environment, holding the one real secret. Kept apart from `config.toml` on purpose —
 * this one is mode-0600 and root-owned, that one is not.
 *
 * Linux only as a *file*: Windows has no `EnvironmentFile=`, so the Windows installer puts the same
 * two variables in the service's own registry key instead (`deploy/edge/README.md`).
 *
 * @param {import("./installers.d.mts").InstallerValues} v
 * @returns {string}
 */
export function envFile(v) {
  return [
    "# pos_edge environment — the store's secrets. Install it as root:",
    "#   sudo install -o root -g root -m 0600 env /etc/pos-edge/env",
    "# The service unit reads it via EnvironmentFile=-/etc/pos-edge/env.",
    // Which store this belongs to, in a comment, because nothing else in the file says. An operator
    // replacing a dead machine has four downloads open — and for an estate of shops, two sets of
    // four — and this was the one that named no store. `EnvironmentFile` ignores `#` lines, and the
    // whole file is embedded in a quoted heredoc by the installer, so nothing here is expanded.
    v.storeName ? `# Store:  ${v.storeName}  (${v.storeId})` : `# Store:  ${v.storeId}`,
    "",
    // No key means a commented line, never `POS_EDGE_SYNC_KEY=  # hint`: systemd keeps everything
    // after the `=`, so the hint itself became the key and the cloud refused it (ADR-0143).
    ...(v.key
      ? [
          "# The scoped store key (read_config + relay_orders). Shown once at issuance and not",
          "# recoverable — revoke and re-issue in the console if this file is lost.",
          `POS_EDGE_SYNC_KEY=${v.key}`,
        ]
      : [
          "# No store key is needed: the box syncs with the device credential its activation code",
          "# mints. To use a store key instead, uncomment this line and paste the key after the =.",
          "# POS_EDGE_SYNC_KEY=",
        ]),
    "",
    "# Optional. Without it the box publishes its events over HTTPS to the cloud with its device",
    "# credential. Set it only if your cloud runs the NATS stream for store events: config.toml",
    "# already names the stream and the subject, and this line carries the broker token.",
    "#",
    "# The console cannot fill it in. Unlike the store key above, the NATS token is ONE secret shared",
    "# by the whole fleet, held on the cloud box, so putting it in a browser would spread it across",
    "# every machine in the estate. Recover it on the cloud box and uncomment this line:",
    "#",
    "#   sudo sed -n 's/  token: //p' deploy/secrets/nats.conf",
    "#",
    `# POS_EDGE_NATS_URL=tls://:<that token>@${v.cloudHost}:${NATS_CLIENT_PORT}`,
    "#",
    "# The tls:// scheme is what makes the client require TLS; nats:// connects in plaintext and the",
    "# broker refuses it. The token goes in the userinfo exactly as shown.",
    "",
  ].join("\n");
}

/**
 * The artifact a Linux technician actually runs (roadmap-v3 **R3**): a single script that lays the
 * box out correctly instead of asking someone to follow a README at 7am in a restaurant. It embeds
 * `config.toml` and `env` as heredocs and then does exactly what `deploy/edge/pos-edge.service`'s
 * install block documents — no more, no less, so there is one definition of the layout rather than
 * two that drift.
 *
 * The **slot layout** is the reason this is worth generating rather than typing. Since ADR-0055
 * Amendment 1 the unit's `ExecStart` is `/var/lib/pos-edge/bin/current`, a symlink the edge retargets
 * to install its own updates; a box laid out the old way (the binary at `/usr/local/bin/pos-edge`)
 * trades perfectly well and silently **never self-updates**. That is exactly the kind of mistake a
 * hand-typed install makes and nobody notices for a release or two.
 *
 * It is deliberately not a `curl | sh`: the operator downloads it, can read every line, and runs it
 * with `sudo`. Nothing in it reaches the network.
 *
 * @param {import("./installers.d.mts").InstallerValues} v
 * @returns {string}
 */
export function linuxInstaller(v) {
  const port = v.bindPort.trim() || DEFAULT_BIND_PORT;
  return [
    "#!/bin/sh",
    "# pos_edge installer — generated by the new-store wizard for one specific store.",
    `# Store:  ${v.storeName}  (${v.storeId})`,
    `# Tenant: ${v.tenantLabel}  (${v.tenantId})`,
    "#",
    "# WHAT IT DOES, in order: creates the service user and the state directory, puts the binary in",
    "# the first update slot and points `current` at it, writes the bootstrap config and the",
    "# environment file (mode 0600, root-owned), installs the systemd unit, opens the listen port on",
    "# whichever packet filter is actually running, then enables and starts the service. Idempotent:",
    "# running it twice is safe and re-applies the same layout.",
    "#",
    "# THIS FILE CONTAINS THE STORE'S KEY. Treat it as you would a password, and delete it once the",
    "# box is up. Revoke and re-issue in the console if it leaks.",
    "#",
    "# RUN IT AS:  sudo sh install-pos-edge.sh /path/to/pos-edge /path/to/pos-edge.service",
    "",
    "set -eu",
    "",
    'BINARY="${1:?usage: install-pos-edge.sh <pos-edge binary> <pos-edge.service unit>}"',
    'UNIT="${2:?the unit file ships in deploy/edge/pos-edge.service}"',
    "STATE=/var/lib/pos-edge",
    "",
    '[ "$(id -u)" -eq 0 ] || { echo "run me as root (sudo)" >&2; exit 1; }',
    '[ -f "$BINARY" ] || { echo "no such binary: $BINARY" >&2; exit 1; }',
    '[ -f "$UNIT" ] || { echo "no such unit file: $UNIT" >&2; exit 1; }',
    "",
    "# The service account. No login shell and no home: it only ever runs one program.",
    "id -u pos >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin pos",
    "",
    "# The update slot layout (ADR-0055 Amendment 1). `current` is what the unit starts and what the",
    "# edge retargets on a successful update; without it the box never self-updates.",
    "#",
    "# A box that already has `current` is one the edge is managing: it may be running slot-b after",
    "# an over-the-air update, and re-laying slot-a would point `current` back at whatever binary",
    "# this installer was handed — a silent downgrade of a shop that had updated itself. So the",
    "# slots are laid out once, and a re-run refreshes the config, the unit and the rescue copy",
    "# without touching the running binary. That is what makes running this twice safe.",
    'install -d -o pos -g pos "$STATE" "$STATE/bin"',
    'if [ -e "$STATE/bin/current" ]; then',
    '  echo "bin/current exists — leaving the installed binary alone (the edge manages its own updates)"',
    "else",
    '  install -o pos -g pos -m 0755 "$BINARY" "$STATE/bin/slot-a"',
    '  ln -sfn slot-a "$STATE/bin/current"',
    '  chown -h pos:pos "$STATE/bin/current"',
    "fi",
    "",
    "# The operator's rescue copy, and what `pos-edge --self-test` is run from by hand. Not what the",
    "# service runs.",
    'install -o root -g root -m 0755 "$BINARY" /usr/local/bin/pos-edge',
    "",
    "# The bootstrap config: which store this is and which cloud to dial. No credential in it.",
    `cat > "$STATE/config.toml" <<'POS_EDGE_CONFIG'`,
    configToml(v),
    "POS_EDGE_CONFIG",
    'chown pos:pos "$STATE/config.toml"',
    'chmod 0644 "$STATE/config.toml"',
    "",
    "# The environment file: the one real secret. Root-owned, mode 0600, never world-readable.",
    "install -d -o root -g root -m 0755 /etc/pos-edge",
    "cat > /etc/pos-edge/env <<'POS_EDGE_ENV'",
    envFile(v),
    "POS_EDGE_ENV",
    "chown root:root /etc/pos-edge/env",
    "chmod 0600 /etc/pos-edge/env",
    "",
    "# The service.",
    'install -o root -g root -m 0644 "$UNIT" /etc/systemd/system/pos-edge.service',
    "",
    "# Sealed secrets (ADR-0151). The root helper seals a vault key with systemd-creds (with the TPM2",
    "# when there is a usable one), and the unit drop-in has it unsealed before every start; the edge",
    "# seals its device credential under that key, so an activation survives a reboot. The drop-in goes",
    "# in only once a key is sealed, because an edge whose drop-in cannot get it a key answers vault",
    "# errors. Without systemd-creds (systemd before 250), or where no key can be sealed, the",
    "# credential stays in the kernel keyring, which a reboot empties.",
    "VAULT_SEALED=no",
    "if command -v systemd-creds >/dev/null 2>&1; then",
    "  install -d -o root -g root -m 0755 /usr/local/libexec/pos-edge",
    "  cat > /usr/local/libexec/pos-edge/pos-edge-vault <<'POS_EDGE_VAULT'",
    VAULT_HELPER,
    "POS_EDGE_VAULT",
    "  chown root:root /usr/local/libexec/pos-edge/pos-edge-vault",
    "  chmod 0755 /usr/local/libexec/pos-edge/pos-edge-vault",
    "  if /usr/local/libexec/pos-edge/pos-edge-vault seal; then",
    "    VAULT_SEALED=yes",
    "  fi",
    "fi",
    'if [ "$VAULT_SEALED" = yes ]; then',
    "  install -d -o root -g root -m 0755 /etc/systemd/system/pos-edge.service.d",
    "  cat > /etc/systemd/system/pos-edge.service.d/vault.conf <<'POS_EDGE_VAULT_CONF'",
    VAULT_DROPIN,
    "POS_EDGE_VAULT_CONF",
    "  chmod 0644 /etc/systemd/system/pos-edge.service.d/vault.conf",
    "else",
    "  rm -f /etc/systemd/system/pos-edge.service.d/vault.conf",
    '  echo "No vault key was sealed (that needs systemd-creds, from systemd 250): the device credential stays in the kernel keyring, and a reboot means activating again." >&2',
    "fi",
    "",
    "# A restart rather than `enable --now`: on a box already running, this is what applies the unit",
    "# and the drop-in, and what moves a credential still in the kernel keyring into the sealed store",
    "# before the next reboot can empty the keyring.",
    "systemctl daemon-reload",
    "systemctl enable pos-edge",
    "systemctl restart pos-edge",
    "",
    "# The tills have to be able to reach the listen port, and on Linux the honest answer is usually",
    "# that nothing is in the way: a stock Debian or Ubuntu box ships no active packet filter. So this",
    "# only acts on a filter that is *running*. A blind `ufw allow` would be worse than doing nothing:",
    "# on a box where ufw is installed but inactive it silently records a rule that first takes effect",
    "# the day somebody enables the firewall for an unrelated reason, which is a change to the store's",
    "# exposure made months earlier by an installer nobody remembers running.",
    "#",
    "# `ufw status` prints \"Status: inactive\", which contains the word \"active\" — hence the anchored",
    "# match rather than a substring one.",
    `PORT=${port}`,
    "if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -qi '^Status: active'; then",
    '  ufw allow "$PORT/tcp" >/dev/null 2>&1 && echo "ufw: allowed $PORT/tcp" || echo "ufw: could not allow $PORT/tcp - open it by hand or the tills will time out" >&2',
    "elif command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then",
    '  firewall-cmd --permanent --add-port="$PORT/tcp" >/dev/null 2>&1 && firewall-cmd --reload >/dev/null 2>&1 && echo "firewalld: allowed $PORT/tcp" || echo "firewalld: could not allow $PORT/tcp - open it by hand or the tills will time out" >&2',
    "else",
    '  echo "no active ufw or firewalld: assuming nothing filters $PORT/tcp on this box"',
    "fi",
    "",
    "systemctl --no-pager --lines=0 status pos-edge || true",
    "echo",
    `echo "pos_edge installed for ${shDouble(v.storeName)} (${v.storeId})."`,
    `echo "Next: open http://<this box>:${port}/ on a device on the shop LAN and pair it."`,
    ...(v.key
      ? ['echo "Now DELETE this installer — it contains the store key."']
      : [
          'echo "No store key was issued, and none is needed: once this box is activated on /setup, it syncs with the credential activation gives it."',
        ]),
    "",
  ].join("\n");
}

/**
 * The lines every Windows installer shares, whichever way it got its values.
 *
 * Kept as one function because the alternative — a checked-in `.ps1` beside a generator that emits
 * the same script — is two definitions of a service registration that must stay in step, and the
 * one that drifts is the one nobody runs until a store will not come up. `deploy/edge/install-pos-edge.ps1`
 * is emitted from here and CI regenerates it to prove it still matches.
 *
 * @param {object} parts
 * @param {readonly string[]} parts.configBlock  lines that leave `$config` holding the TOML
 * @param {readonly string[]} parts.keyBlock     lines that add the store key to `$environment`, if any
 * @param {readonly string[]} parts.doneBlock    the closing "installed for …" line
 * @param {readonly string[]} parts.warningBlock the closing key-handling warning
 * @param {string} parts.cloudHost
 * @param {string} parts.bindPort
 * @param {string} parts.storeId   a PowerShell expression for the store's id, which /healthz must echo
 * @param {string} parts.cloudUrl  a PowerShell expression for the cloud's origin, probed before /setup
 * @param {string} parts.carriedVersion  a PowerShell expression for the release `$Binary` is, or `''`
 * @returns {string[]}
 */
function windowsBody({ configBlock, keyBlock, doneBlock, warningBlock, cloudHost, bindPort, storeId, cloudUrl, carriedVersion }) {
  return [
    "$service = 'pos-edge'",
    `$expectedStore = ${storeId}`,
    `$cloudOrigin = ${cloudUrl}`,
    `$installerVersion = ${carriedVersion}`,
    "",
    "# What the summary at the end reports: every step that can leave a store unreachable adds one line",
    "# saying what happened and what to do, so a technician need not read the scroll-back.",
    "$report = New-Object System.Collections.Generic.List[object]",
    "function Add-Check([string] $Level, [string] $Text) {",
    "    $report.Add([pscustomobject]@{ Level = $Level; Text = $Text })",
    "}",
    "",
    "if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {",
    "    throw \"no such binary: $Binary\"",
    "}",
    "$Binary = (Resolve-Path -LiteralPath $Binary).Path",
    "",
    "# The state directory and the update slot layout (ADR-0055 Amendment 1). `current` is what the",
    "# service runs and what the edge retargets on a successful update; without it the box never",
    "# self-updates.",
    "#",
    "# A box that already has `current` is one the edge is managing: it may be running slot-b after an",
    "# over-the-air update, and re-laying slot-a would point `current` back at whatever binary this",
    "# installer was handed — a silent downgrade of a shop that had updated itself. So the slots are",
    "# laid out once, and a re-run refreshes the config, the service registration and the rescue copy",
    "# without touching the running binary. That is what makes running this twice safe.",
    "$bin = Join-Path $Root 'bin'",
    "New-Item -ItemType Directory -Force -Path $Root, $bin | Out-Null",
    "",
    "$current = Join-Path $bin 'current'",
    "$kept = Test-Path -LiteralPath $current",
    "if ($kept) {",
    "    Write-Host 'bin\\current exists — leaving the installed binary alone (the edge manages its own updates)'",
    "} else {",
    "    Copy-Item -LiteralPath $Binary -Destination (Join-Path $bin 'slot-a') -Force",
    "    # Needs SeCreateSymbolicLinkPrivilege, which an elevated shell has. The edge creates the same",
    "    # link with CreateSymbolicLinkW when it installs an update.",
    "    New-Item -ItemType SymbolicLink -Path $current -Target (Join-Path $bin 'slot-a') -Force | Out-Null",
    "}",
    "",
    "# The operator's rescue copy, and what `pos-edge.exe --self-test` is run from by hand. Not what",
    "# the service runs.",
    "Copy-Item -LiteralPath $Binary -Destination (Join-Path $Root 'pos-edge.exe') -Force",
    "",
    "# The bootstrap config: which store this is and which cloud to dial. No credential in it.",
    ...configBlock,
    "$configPath = Join-Path $Root 'config.toml'",
    "# UTF-8 without a BOM: the TOML parser reads a leading BOM as part of the first key and refuses",
    "# the file. Set-Content -Encoding utf8 writes one on Windows PowerShell 5.1, which is what ships.",
    "[System.IO.File]::WriteAllText($configPath, $config, (New-Object System.Text.UTF8Encoding $false))",
    "",
    "# THE PORT, WHICH ON WINDOWS IS NOT OPEN.",
    "#",
    "# Defender Firewall drops an inbound connection to a port no rule names, silently and with no",
    "# log line on either side. So a Windows store installed correctly in every other respect comes",
    "# up, opens its database, writes its pairing URL — and every till on the floor gets a connection",
    "# timeout. That failure is indistinguishable from a broken install, which is why it belongs here",
    "# rather than as a step in a runbook for a technician to type and mis-type.",
    "#",
    "# Private profile only. This port is plain HTTP on the shop LAN, so a rule on the Public profile",
    "# would offer the till API to whatever network the box is plugged into next. Domain is left alone",
    "# deliberately: an estate that joins its boxes to a domain manages this with group policy, and an",
    "# installer should not quietly compete with it.",
    "#",
    "# Idempotent by removal, not by skip: re-running with a different port has to *move* the rule",
    "# rather than leave the old port open beside the new one. A firewall that is switched off, driven",
    "# by policy, or absent from this Windows build is not an install failure - it is warned about and",
    "# skipped, because the service itself is registered and running either way.",
    `$firewallPort = "${bindPort}"`,
    "$firewallRule = \"pos-edge (TCP $firewallPort)\"",
    "if (Get-Command -Name New-NetFirewallRule -ErrorAction SilentlyContinue) {",
    "    try {",
    "        # Matches every port this installer has ever opened, not just the one it is opening now.",
    "        $stale = @(Get-NetFirewallRule -DisplayName 'pos-edge (TCP *)' -ErrorAction SilentlyContinue)",
    "        foreach ($rule in $stale) { Remove-NetFirewallRule -Name $rule.Name -ErrorAction Stop }",
    "        $firewall = @{",
    "            DisplayName = $firewallRule",
    "            Direction   = 'Inbound'",
    "            Action      = 'Allow'",
    "            Protocol    = 'TCP'",
    "            LocalPort   = $firewallPort",
    "            Profile     = 'Private'",
    "            Description = 'Pizza 4P''s POS edge: tills and kitchen displays on the shop LAN.'",
    "        }",
    "        New-NetFirewallRule @firewall | Out-Null",
    "        Write-Host \"firewall: inbound TCP $firewallPort allowed on the Private profile\"",
    "    } catch {",
    "        Write-Warning \"could not open TCP $firewallPort ($($_.Exception.Message)). The service will run, but no device on the LAN will reach it until that port is open.\"",
    "    }",
    "} else {",
    "    Write-Warning \"New-NetFirewallRule is not available on this Windows build. Open inbound TCP $firewallPort by hand, or no till will reach this box.\"",
    "}",
    "",
    "# The service. `sc.exe create` fails if it already exists, so a re-run reconfigures instead —",
    "# which is also how the binPath is corrected if the layout moved.",
    "$exists = $null -ne (Get-Service -Name $service -ErrorAction SilentlyContinue)",
    "if ($exists) {",
    "    & sc.exe config $service binPath= \"`\"$current`\"\" start= auto | Out-Null",
    "} else {",
    "    & sc.exe create $service binPath= \"`\"$current`\"\" start= auto | Out-Null",
    "}",
    "& sc.exe description $service \"Pizza 4P's POS edge (store server)\" | Out-Null",
    "",
    "# WITHOUT THESE TWO PATHS A WINDOWS STORE CANNOT BE PAIRED WITH AT ALL (ADR-0117).",
    "#",
    "# A service started by the Service Control Manager has no console, so everything the edge logs to",
    "# standard output is discarded — including the pairing URL, which is minted once per process start",
    "# and which no route mints a second time. The two variables below are what make a headless box",
    "# diagnosable and pairable:",
    "#",
    "#   * POS_EDGE_LOG_FILE tees the log into a file (the current run, plus the previous run beside it",
    "#     as pos-edge.log.1). It never contains the pairing code, the staff sign-in stream or a",
    "#     credential: the writer excludes those, and raising RUST_LOG cannot change it;",
    "#   * POS_EDGE_PAIRING_FILE is where the pairing URL itself is written, and the edge DELETES it the",
    "#     moment a device redeems the code. Treat it as a five-minute password: read it, pair, and it",
    "#     is gone.",
    "$logPath = Join-Path $Root 'pos-edge.log'",
    "$pairingPath = Join-Path $Root 'pairing-url.txt'",
    "",
    "# The environment, service-scoped rather than machine-wide. A machine environment variable is",
    "# readable by every local administrator and shows up in process listings of unrelated services;",
    "# this key is readable only by accounts that can read the service. REG_MULTI_SZ is how SCM passes",
    "# several variables to one service. SCM reads it at start, not on the fly, which is why the",
    "# restart below is not optional.",
    "$environment = @(",
    "    \"POS_EDGE_CONFIG=$configPath\",",
    "    \"POS_EDGE_LOG_FILE=$logPath\",",
    "    \"POS_EDGE_PAIRING_FILE=$pairingPath\",",
    "    'RUST_LOG=info'",
    ")",
    ...keyBlock,
    "",
    "# Without an event-bus URL the box publishes its events over HTTPS to the cloud with its device",
    "# credential. Add one only if your cloud runs the NATS stream for store events. It is ONE secret",
    "# shared by the whole fleet, held on the cloud box, so the console cannot fill it in without",
    "# spreading it across every machine in the estate. Recover it on the cloud box, then add it here",
    "# and restart:",
    "#",
    `#   $env = 'POS_EDGE_NATS_URL=tls://:<that token>@${cloudHost}:${NATS_CLIENT_PORT}'`,
    "",
    "$key = \"HKLM:\\SYSTEM\\CurrentControlSet\\Services\\$service\"",
    "New-ItemProperty -Path $key -Name 'Environment' -PropertyType MultiString -Value $environment -Force | Out-Null",
    "",
    "# NOT OPTIONAL. This is the Windows counterpart of the unit's Restart=always, and without it the",
    "# store does not come back from an update or a crash. SCM has no always-restart setting — it has",
    "# failure actions, applied when a service looks like it failed:",
    "#",
    "#   * an install retargets bin\\current and exits 1, which SCM reads as a failure and restarts",
    "#     five seconds later on the new binary. Exiting 0 would look like a deliberate stop and the",
    "#     shop would stay dark until somebody drove there;",
    "#   * an operator's `sc.exe stop` exits 0 and the service stays stopped, which is what was asked;",
    "#   * a failed start — unreadable config, port already bound, a database that will not open —",
    "#     also exits 1, so the same action retries it rather than leaving a dead service.",
    "#",
    "# reset= 86400 clears the failure count after a day, so three bad days do not exhaust the list.",
    "& sc.exe failure $service reset= 86400 actions= restart/5000/restart/5000/restart/30000 | Out-Null",
    "",
    "# Restart rather than start: on a re-run the service is already up on the old environment.",
    "# `sc.exe stop` returns once SCM has sent the control, not once the edge has drained, and a start",
    "# sent while the old process is still stopping is refused. So this waits for STOPPED, bounded; a",
    "# service that has not stopped by then is reported and started anyway.",
    "if ($exists) {",
    "    & sc.exe stop $service | Out-Null",
    "    try {",
    "        (Get-Service -Name $service).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60))",
    "    } catch {",
    "        Add-Check 'WARN' \"the old $service process did not stop within 60 seconds, so the start below may be refused. If the service is not running afterwards: sc.exe start $service\"",
    "    }",
    "}",
    "",
    "# The pairing file the previous process wrote names a code only that process could redeem, and",
    "# nothing deletes it when the service stops. Left in place, the wait below takes it for this",
    "# start's file: it prints a dead code and moves on before the new process is listening.",
    "Remove-Item -LiteralPath $pairingPath -Force -ErrorAction SilentlyContinue",
    "",
    "# A program already listening on the port is a failure the edge can only report by exiting and",
    "# being restarted, over and over. With the service stopped, whatever holds the port now is",
    "# something else, and it is named here, before the start, with what to do about it.",
    "if (Get-Command -Name Get-NetTCPConnection -ErrorAction SilentlyContinue) {",
    `    foreach ($holder in @(Get-NetTCPConnection -State Listen -LocalPort ${bindPort} -ErrorAction SilentlyContinue)) {`,
    "        $process = Get-Process -Id $holder.OwningProcess -ErrorAction SilentlyContinue",
    "        $name = if ($null -ne $process) { $process.ProcessName } else { 'an unknown program' }",
    `        Add-Check 'FAIL' "port ${bindPort} is already in use by $name (PID $($holder.OwningProcess)), so pos-edge cannot listen on it. Stop that program, or give this store another port."`,
    "    }",
    "}",
    "",
    "$startOutput = (& sc.exe start $service | Out-String).Trim()",
    "# 1056: already running, which is what a start racing a slow stop answers; the checks below say",
    "# whether the store came up.",
    "if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 1056) {",
    "    Add-Check 'FAIL' \"sc.exe start $service failed with code $($LASTEXITCODE): $startOutput\"",
    "}",
    "",
    "& sc.exe query $service",
    "Write-Host ''",
    ...doneBlock,
    "Write-Host ''",
    "# The pairing URL, read off the box. The service was started seconds ago, so the code in it is",
    "# live for five minutes from that start; if it has expired, `sc.exe stop pos-edge` and",
    "# `sc.exe start pos-edge` mints a new one. Printed here rather than left for the operator to find,",
    "# because it is the one step between an installed service and a till that works.",
    "#",
    "# The wait is what makes the happy path the one that prints a URL: `sc.exe start` returns as soon",
    "# as SCM has accepted the start, and the edge writes this file after it has opened the store and",
    "# replayed the log, which on a shop-floor PC with a season of history is not instant. Bounded, and",
    "# a timeout falls through to the else branch rather than failing the install — the service is",
    "# registered either way.",
    "$deadline = (Get-Date).AddSeconds(30)",
    "while (-not (Test-Path -LiteralPath $pairingPath) -and (Get-Date) -lt $deadline) {",
    "    Start-Sleep -Milliseconds 500",
    "}",
    "",
    "# Which store and release answer on the port. `bin\\current` may be a binary the edge installed",
    "# over the air rather than the one this installer was handed, so this is the one place that can",
    "# say what the store runs. It is also what the setup page waits for: SCM reports RUNNING before",
    "# the edge has opened its store and bound the port. Bounded like the wait above.",
    "$health = $null",
    "$deadline = (Get-Date).AddSeconds(30)",
    "while ($null -eq $health -and (Get-Date) -lt $deadline) {",
    `    try { $health = Invoke-RestMethod -Uri "http://127.0.0.1:${bindPort}/healthz" -TimeoutSec 2 } catch { Start-Sleep -Milliseconds 500 }`,
    "}",
    "if ($null -ne $health) {",
    "    $version = $health.PSObject.Properties['version']",
    "    $version = if ($null -ne $version) { $version.Value } else { 'unknown' }",
    "    $answering = $health.PSObject.Properties['store_id']",
    "    if ($null -ne $answering -and $answering.Value -ne $expectedStore) {",
    `        Add-Check 'FAIL' "port ${bindPort} is answered by pos-edge $version for store $($answering.Value), not for $expectedStore. Another pos-edge is running on this PC: stop it, then start the service again (sc.exe start $service)."`,
    "    } else {",
    `        Add-Check 'ok' "pos-edge $version is answering on port ${bindPort} for store $expectedStore"`,
    "    }",
    "    if ($kept) {",
    "        # Which is newer decides the advice: rolling out an older release is a downgrade, never a fix.",
    "        $newer = $false",
    "        try { $newer = [version]$installerVersion -gt [version]$version } catch { $newer = $false }",
    "        if ($installerVersion -and $newer) {",
    "            Add-Check 'note' \"this PC already had pos-edge and runs $version, which was kept; the $installerVersion this installer carries went to $Root\\pos-edge.exe only. To run $installerVersion here, roll it out from the console's OTA screen.\"",
    "        } elseif ($installerVersion -and $installerVersion -ne $version) {",
    "            Add-Check 'note' \"this PC already had pos-edge and runs $version, newer than the $installerVersion this installer carries, so it was kept.\"",
    "        } else {",
    "            Add-Check 'note' \"this PC already had pos-edge, so the binary it runs was kept (version $version). A newer release reaches this PC over the air, from the console's OTA screen.\"",
    "        }",
    "    }",
    "} else {",
    "    $state = Get-Service -Name $service -ErrorAction SilentlyContinue",
    "    $state = if ($null -ne $state) { [string]$state.Status } else { 'not registered' }",
    `    Add-Check 'FAIL' "nothing answered on port ${bindPort} within 30 seconds, and the service is $state. The last lines of $logPath are printed above; the first error in them is the cause."`,
    "    if (Test-Path -LiteralPath $logPath) {",
    "        Write-Host \"---- the last 20 lines of $logPath ----\"",
    "        Get-Content -LiteralPath $logPath -Tail 20 | ForEach-Object { Write-Host $_ }",
    "        Write-Host '----'",
    "    }",
    "}",
    "",
    "# The adapters a till can reach this PC through: up, and with a default gateway, which is the shop",
    "# LAN and not a VPN or a virtual switch. The pairing URL is completed with their addresses below,",
    "# and theirs are the only firewall profiles that matter.",
    "$lanConfigs = @()",
    "if (Get-Command -Name Get-NetIPConfiguration -ErrorAction SilentlyContinue) {",
    "    try {",
    "        $lanConfigs = @(Get-NetIPConfiguration | Where-Object {",
    "            $_.IPv4DefaultGateway -and $_.IPv4Address -and $_.NetAdapter -and $_.NetAdapter.Status -eq 'Up'",
    "        })",
    "    } catch {",
    "        $lanConfigs = @()",
    "    }",
    "}",
    "$lanIndexes = @($lanConfigs | ForEach-Object { $_.InterfaceIndex })",
    "$lan = @($lanConfigs | ForEach-Object { $_.IPv4Address.IPAddress })",
    "",
    "# The firewall rule above is on the Private profile only, and Windows puts a network it has not",
    "# been told about on Public. A store whose LAN is Public passes every check on this PC and no till",
    "# can reach it, which is the failure a technician is least likely to guess. A VPN adapter on",
    "# Public is how it should be, and saying otherwise would teach the technician to ignore this line.",
    "if (Get-Command -Name Get-NetConnectionProfile -ErrorAction SilentlyContinue) {",
    "    $lanProfiles = @(Get-NetConnectionProfile -ErrorAction SilentlyContinue | Where-Object { $lanIndexes -contains $_.InterfaceIndex })",
    "    foreach ($connection in $lanProfiles) {",
    "        if ([string]$connection.NetworkCategory -eq 'Public') {",
    `            Add-Check 'WARN' "network '$($connection.Name)' ($($connection.InterfaceAlias)) is Public, and port ${bindPort} is open on Private networks only: no till can reach this PC until it is Private. Fix: Set-NetConnectionProfile -InterfaceIndex $($connection.InterfaceIndex) -NetworkCategory Private"`,
    "        } elseif ([string]$connection.NetworkCategory -eq 'DomainAuthenticated') {",
    `            Add-Check 'WARN' "network '$($connection.Name)' is a domain network, where this installer leaves the firewall to group policy: port ${bindPort} must be allowed there."`,
    "        }",
    "    }",
    "}",
    "",
    "# Activation needs this PC to reach the cloud over HTTPS, and a TLS handshake fails on a clock that",
    "# is far off. Both are checked now, while someone is at the PC, rather than discovered at /setup.",
    "# Any HTTP answer counts as reachable: it is the network that is in question, not the route.",
    "$cloudHealth = $cloudOrigin.TrimEnd('/') + '/health'",
    "[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12",
    "$cloudDate = $null",
    "try {",
    "    $probe = Invoke-WebRequest -Uri $cloudHealth -UseBasicParsing -TimeoutSec 10",
    "    $cloudDate = $probe.Headers['Date']",
    "    Add-Check 'ok' \"the cloud answers at $cloudOrigin\"",
    "} catch {",
    "    $response = $_.Exception.PSObject.Properties['Response']",
    "    if ($null -ne $response -and $null -ne $response.Value) {",
    "        # Windows PowerShell 5.1 hands back a WebResponse, whose headers index by name; PowerShell 7",
    "        # an HttpResponseMessage, whose headers do not. The clock check is a bonus, so it is skipped.",
    "        try { $cloudDate = [string]$response.Value.Headers['Date'] } catch { $cloudDate = $null }",
    "        Add-Check 'ok' \"the cloud answers at $cloudOrigin\"",
    "    } else {",
    "        Add-Check 'FAIL' \"cannot reach the cloud at $cloudOrigin ($($_.Exception.Message)). Activation needs this PC to reach it over HTTPS: check the internet connection, a proxy or a firewall.\"",
    "    }",
    "}",
    "if ($cloudDate) {",
    "    try {",
    "        $skew = [Math]::Abs(((Get-Date).ToUniversalTime() - [DateTime]::Parse([string]$cloudDate).ToUniversalTime()).TotalMinutes)",
    "        if ($skew -gt 5) {",
    "            Add-Check 'WARN' \"this PC's clock is $([Math]::Round($skew)) minutes away from the cloud's; activation and every TLS connection can fail until it is right. Fix: Settings > Time & language > Sync now.\"",
    "        }",
    "    } catch {}",
    "}",
    "",
    "Write-Host 'Next: pair a device on the shop LAN. Open the URL below on it.'",
    "if (Test-Path -LiteralPath $pairingPath) {",
    "    $pairing = (Get-Content -LiteralPath $pairingPath -Raw).Trim()",
    "    # The edge writes the path alone when it cannot name its own address: it listens on every",
    "    # interface and no advertised_ip is set. A device cannot open a bare path, and this box can name",
    "    # its addresses, so the URL is completed with each one that has a default gateway (the shop LAN,",
    "    # not a virtual switch), or with a placeholder that says where to look.",
    "    $urls = @($pairing)",
    "    if ($pairing.StartsWith('/')) {",
    `        $urls = @("http://<this PC's IPv4 address, from ipconfig>:${bindPort}$pairing")`,
    "        if ($lan.Count -gt 0) {",
    `            $urls = @($lan | ForEach-Object { "http://$($_):${bindPort}$pairing" })`,
    "        } else {",
    "            Add-Check 'WARN' 'no network adapter with a default gateway is up, so nothing on the shop LAN can reach this PC yet. Connect it to the shop network, then read the pairing file again.'",
    "        }",
    "    }",
    "    Write-Host ''",
    "    foreach ($url in $urls) { Write-Host $url }",
    "    Write-Host ''",
    "    Write-Host \"That code lives five minutes and pairs one device. For the next device: sc.exe stop $service; sc.exe start $service — then read $pairingPath again.\"",
    "} else {",
    // Double-quoted, because the parameterised variant passes a PowerShell variable here and a
    // single-quoted string would print the variable's name to the technician instead of the port.
    `    Write-Host "The pairing URL is not there yet. Read $pairingPath in a moment, or ${"$"}logPath for why the service did not get that far; the address is http://<this box>:${bindPort}/."`,
    "    Add-Check 'WARN' \"no pairing code was written within 30 seconds; $pairingPath appears once the store is up.\"",
    "}",
    "",
    "# Everything above, in one place: a technician reads this and not the scroll-back. Nothing here",
    "# fails the install, because the service is registered either way and most of these are fixed",
    "# on the PC, not by running the installer again.",
    "Write-Host ''",
    "Write-Host 'pos-edge setup summary'",
    "foreach ($check in $report) {",
    "    $colour = switch ($check.Level) { 'ok' { 'Green' } 'FAIL' { 'Red' } 'WARN' { 'Yellow' } default { 'Gray' } }",
    "    Write-Host ('  {0,-5} {1}' -f $check.Level, $check.Text) -ForegroundColor $colour",
    "}",
    "$failures = @($report | Where-Object { $_.Level -eq 'FAIL' }).Count",
    "if ($failures -gt 0) {",
    "    Write-Host \"  $failures problem(s) above stop this store from working; each line says what to do.\" -ForegroundColor Red",
    "}",
    "Write-Host ''",
    ...warningBlock,
    "",
  ];
}

/**
 * The header both Windows installers share: the elevation requirement, the parameters every one
 * takes, and the strict-mode preamble.
 *
 * @param {readonly string[]} help  the comment-based help block, without its `<#` / `#>`
 * @param {readonly string[]} extraParams  parameters this variant adds, already comma-terminated
 * @param {string} root
 * @returns {string[]}
 */
function windowsHeader(help, extraParams, root) {
  return [
    "#Requires -RunAsAdministrator",
    "<#",
    ...help,
    "#>",
    "[CmdletBinding()]",
    "param(",
    "    [Parameter(Mandatory = $true)]",
    "    [string] $Binary,",
    "",
    ...extraParams,
    `    [string] $Root = '${root}'`,
    ")",
    "",
    "Set-StrictMode -Version Latest",
    "$ErrorActionPreference = 'Stop'",
    "",
  ];
}

/**
 * The same handoff for a Windows store (roadmap-v3 **R4**, closing
 * [issue #182](https://github.com/buitrungtuan-4ps/pos-framework-4ps/issues/182)).
 *
 * Windows used to get the two files and a README, so the install was five `sc.exe` lines typed by
 * hand — and the one easiest to skip, `sc.exe failure`, is the one that decides whether the box
 * comes back from an over-the-air update. SCM has no `Restart=always`; it has failure actions, and
 * an install deliberately exits `1` so that they fire. A store that skipped that line installs its
 * update and stays dark until somebody drives there.
 *
 * Three things differ from the Linux script, and each is a property of the platform rather than a
 * translation choice:
 *
 *  * **No service account.** `sc.exe create` with no `obj=` runs the service as `LocalSystem`, which
 *    holds `SeCreateSymbolicLinkPrivilege` — the privilege the update slot layout needs. A named
 *    low-privilege account does not have it unless it was granted, and the edge's response to a
 *    missing `bin\\current` is to not update at all rather than to fail every ten minutes.
 *  * **No `env` file.** There is no `EnvironmentFile=` equivalent, so the two variables go in the
 *    service's own registry key as a `REG_MULTI_SZ`, readable only by accounts that can read that
 *    key — not by every process on the box, which is what a machine-wide `setx … /M` would mean.
 *  * **An absolute `store_path`.** See [`WINDOWS_ROOT`].
 *
 * @param {import("./installers.d.mts").InstallerValues} v
 * @returns {string}
 */
export function windowsInstaller(v) {
  const root = v.windowsRoot ?? WINDOWS_ROOT;
  return PS_BOM + [
    ...windowsHeader(
      [
        ".SYNOPSIS",
        "    pos_edge installer — generated by the new-store wizard for one specific store.",
        "",
        ".DESCRIPTION",
        `    Store:  ${v.storeName}  (${v.storeId})`,
        `    Tenant: ${v.tenantLabel}  (${v.tenantId})`,
        "",
        "    WHAT IT DOES, in order: creates the state directory, puts the binary in the first update",
        "    slot and points `current` at it, writes the bootstrap config, opens the listen port on the",
        "    Private firewall profile, registers the service with the Service Control Manager, sets the",
        "    service-scoped environment (including the store key), sets the failure actions that are what",
        "    bring the box back after an update, and starts it. Idempotent: running it twice is safe and",
        "    re-applies the same layout.",
        "",
        "    THIS FILE CONTAINS THE STORE'S KEY. Treat it as you would a password, and delete it once",
        "    the box is up. Revoke and re-issue in the console if it leaks.",
        "",
        ".PARAMETER Binary",
        "    Path to pos-edge.exe.",
        "",
        ".PARAMETER Root",
        "    Where the store's state lives. Defaults to the ProgramData path the config below names;",
        "    change both together or the service will not find its database.",
        "",
        ".EXAMPLE",
        "    powershell -ExecutionPolicy Bypass -File .\\install-pos-edge.ps1 -Binary .\\pos-edge.exe",
      ],
      [],
      root,
    ),
    ...windowsBody({
      configBlock: [
        "$config = @'",
        configToml({ ...v, storePath: `${root}\\store.sqlite` }),
        "'@",
      ],
      keyBlock: v.key
        ? [
            "# The scoped store key (read_config + relay_orders), shown once at issuance. The keyring is",
            "# the better home for it (ADR-0086) and this is the headless bring-up override, exactly as",
            "# POS_EDGE_SYNC_KEY is on Linux.",
            `$environment += 'POS_EDGE_SYNC_KEY=${psSingle(v.key)}'`,
          ]
        : [
            "# No key was issued with this store, and none is needed: once the box is activated at",
            "# /setup it syncs with the device credential its activation code mints.",
          ],
      doneBlock: [
        `Write-Host 'pos_edge installed for ${psSingle(v.storeName)} (${v.storeId}).'`,
      ],
      warningBlock: v.key
        ? ["Write-Host 'Now DELETE this installer — it contains the store key.'"]
        : [
            "Write-Host 'No store key was issued, and none is needed: once this box is activated on /setup, it syncs with the credential activation gives it.'",
          ],
      cloudHost: v.cloudHost,
      bindPort: v.bindPort.trim() || DEFAULT_BIND_PORT,
      storeId: `'${v.storeId}'`,
      cloudUrl: `'${psSingle(v.cloudUrl)}'`,
      // The wizard hands the technician this script and a binary they download apart from it.
      carriedVersion: "''",
    }),
  ].join("\n");
}

/**
 * The same installer for a store the console did not create — a fork bringing a box up by hand, or
 * an estate that provisions before it has a cloud to provision from.
 *
 * It is the *same script*: emitted from [`windowsBody`], checked in as
 * `deploy/edge/install-pos-edge.ps1`, and regenerated by CI to prove it has not drifted from the one
 * the wizard hands out. What differs is only where the values come from — parameters typed on the
 * command line instead of values baked in — which is the whole of "a fork supplies values rather
 * than writing code".
 *
 * @returns {string}
 */
export function windowsInstallerTemplate() {
  const root = WINDOWS_ROOT;
  return PS_BOM + [
    "# GENERATED FILE — do not edit.",
    "#",
    "# Emitted by dashboard/src/installers.mjs (windowsInstallerTemplate) and regenerated by the",
    "# `dashboard` CI job, which fails if this file and that generator disagree. Edit the generator.",
    "#",
    "# This is the same installer the new-store wizard hands out, with the store's values taken as",
    "# parameters rather than baked in — for a box the console did not create.",
    "",
    ...windowsHeader(
      [
        ".SYNOPSIS",
        "    pos_edge installer for one Windows store.",
        "",
        ".DESCRIPTION",
        "    WHAT IT DOES, in order: creates the state directory, puts the binary in the first update",
        "    slot and points `current` at it, writes the bootstrap config, opens the listen port on the",
        "    Private firewall profile, registers the service with the Service Control Manager, sets the",
        "    service-scoped environment (including the store key), sets the failure actions that are what",
        "    bring the box back after an update, and starts it. Idempotent: running it twice is safe and",
        "    re-applies the same layout.",
        "",
        "    -SyncKey IS A SECRET. Prefer passing it interactively over leaving it in shell history.",
        "",
        ".PARAMETER Binary",
        "    Path to pos-edge.exe.",
        "",
        ".PARAMETER StoreId",
        "    The store's ULID, from the console's Stores screen.",
        "",
        ".PARAMETER CloudUrl",
        "    The cloud's origin, e.g. https://cloud.example.com.",
        "",
        ".PARAMETER SyncKey",
        "    Optional: a store API key (read_config + relay_orders) for the sync loops to present.",
        "    Omit it and the box syncs with the device credential its activation code mints.",
        "",
        ".PARAMETER BindPort",
        `    The port the edge listens on. Defaults to ${DEFAULT_BIND_PORT}.`,
        "",
        ".PARAMETER Root",
        "    Where the store's state lives.",
        "",
        ".PARAMETER CarriedVersion",
        "    The release -Binary is, which the one-file installer (pos-edge install) passes. The summary",
        "    names it beside the release that is running when a re-run keeps the one already there.",
        "",
        ".PARAMETER OpenSetup",
        "    Open this box's activation screen in the browser when the service is up. The one-file",
        "    installer (pos-edge install, ADR-0140) passes it; a remote shell has no browser to open.",
        "",
        ".PARAMETER AskSyncKey",
        "    With no -SyncKey, ask for a store key in this window. Nothing passes it any more: the",
        "    device credential is enough (ADR-0143). A script run unattended must not stop to ask.",
        "",
        ".EXAMPLE",
        "    powershell -ExecutionPolicy Bypass -File .\\install-pos-edge.ps1 `",
        "        -Binary .\\pos-edge.exe -StoreId 01J... -CloudUrl https://cloud.example.com",
      ],
      [
        "    [Parameter(Mandatory = $true)]",
        "    [string] $StoreId,",
        "",
        "    [Parameter(Mandatory = $true)]",
        "    [string] $CloudUrl,",
        "",
        "    [string] $SyncKey = '',",
        "",
        `    [string] $BindPort = '${DEFAULT_BIND_PORT}',`,
        "",
        "    [string] $CarriedVersion = '',",
        "",
        "    [switch] $OpenSetup,",
        "",
        "    [switch] $AskSyncKey,",
        "",
      ],
      root,
    ),
    ...windowsBody({
      // A double-quoted here-string, so $StoreId and the rest are substituted. The TOML below holds
      // no other `$`, which is what makes that safe.
      configBlock: [
        "$config = @\"",
        configToml({
          storeName: "",
          storeId: "$StoreId",
          tenantLabel: "",
          tenantId: "",
          cloudUrl: "$CloudUrl",
          cloudHost: "",
          bindPort: "$BindPort",
          key: null,
          storePath: `${root}\\store.sqlite`,
        })
          // A double-quoted here-string makes ` the escape character, so a literal one is doubled.
          // Without this the prose backticks in the comments below would vanish from the file the
          // operator actually reads.
          .replace(/`/gu, "``")
          // The one place the parameterised form has to differ: `bind` is always written, because a
          // parameter with a default is always present, and a commented-out line would ignore it.
          .replace(
            `# Optional — override the listen address (default 0.0.0.0:${DEFAULT_BIND_PORT}):\n# bind = "0.0.0.0:$BindPort"`,
            'bind = "0.0.0.0:$BindPort"',
          ),
        "\"@",
      ],
      keyBlock: [
        "# Asked for only when -AskSyncKey is given: pasted into this elevated window, the key goes only",
        "# into the service's registry key below — never onto the network, and never into shell history.",
        "if (-not $SyncKey -and $AskSyncKey) {",
        "    $entered = Read-Host -Prompt 'Paste the store key from the console, or press Enter to skip' -AsSecureString",
        "    $SyncKey = [System.Net.NetworkCredential]::new('', $entered).Password.Trim()",
        "}",
        "",
        "# An optional store key (read_config + relay_orders). The keyring is the better home for it",
        "# (ADR-0086) and this is the headless bring-up override, exactly as POS_EDGE_SYNC_KEY is on",
        "# Linux. Without one the box syncs with its device credential once it is activated (ADR-0143).",
        "if ($SyncKey) {",
        "    $environment += \"POS_EDGE_SYNC_KEY=$SyncKey\"",
        "}",
      ],
      doneBlock: ["Write-Host \"pos_edge installed for $StoreId.\""],
      warningBlock: [
        "if ($SyncKey) { Write-Host 'The store key is now in the service registry key. Clear it from your shell history.' }",
        "",
        "# The next step is activation, on this box's own screen (ADR-0050). The one-file installer asks",
        "# for it; a technician on a remote shell has no browser here, so it is a switch, not a default.",
        "if ($OpenSetup) {",
        "    $setupUrl = \"http://localhost:$BindPort/setup\"",
        "    Write-Host \"Next: activate this store at $setupUrl with the code from the console.\"",
        "    try { Start-Process $setupUrl } catch { Write-Host \"Open $setupUrl in a browser on this PC.\" }",
        "}",
      ],
      cloudHost: "<your cloud host>",
      bindPort: "$BindPort",
      storeId: "$StoreId",
      cloudUrl: "$CloudUrl",
      carriedVersion: "$CarriedVersion",
    }),
  ].join("\n");
}

// ---------------------------------------------------------------------------
// The print agent (ADR-0112).
// ---------------------------------------------------------------------------

/** Where a print agent's state lives on Windows. */
export const AGENT_WINDOWS_ROOT = "C:\\ProgramData\\pos-print-agent";

/**
 * The print agent's configuration file.
 *
 * Three keys, and ADR-0112 is the reason there are only three: *"No domain code, no configuration of
 * its own, no state a person has to reason about."* Where the edge is, the token proving this device
 * is paired to it, and where to keep the one id per printer.
 *
 * The token is a **credential**, and unlike the store's sync key it has nowhere better to go: the
 * agent holds no keyring integration, and a device token is minted by pairing at the box rather than
 * issued by the console. So it lives in the service's own environment on Windows and in a
 * root-owned env file on Linux, never in this file — which is what keeps the config readable by
 * whoever is diagnosing a printer.
 *
 * @param {{edgeUrl: string, statePath: string}} v
 * @returns {string}
 */
export function printAgentToml(v) {
  return [
    "# pos_print_agent — generated by the console. Safe to read; holds no credential.",
    "#",
    "# The device token is NOT here: it is a credential, and it reaches the agent through the",
    "# environment (POS_PRINT_AGENT_TOKEN on Linux, the service's own registry key on Windows).",
    "",
    "# The store's edge. http on a shop LAN, https for an edge that runs somewhere else",
    "# (ADR-0110 made that a deployment axis, and both are real).",
    `edge_url = "${v.edgeUrl}"`,
    "",
    "# One id per printer: the last job written successfully, so a redelivered job after a lost",
    "# acknowledgement is recognised rather than reprinted (ADR-0112).",
    `state_path = "${v.statePath.replace(/\\/gu, "\\\\")}"`,
    "",
  ].join("\n");
}

/**
 * The Windows installer for one print agent, as a checked-in template.
 *
 * **Template only, and that is a fact about the artifact rather than an omission.** The edge's
 * installer has a per-store variant because the console knows a store's id and cloud URL. It knows
 * neither of this one's two values: a device token is minted by *pairing at the box*
 * ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)), and which edge a terminal talks
 * to is whatever a technician can reach from the shop floor. So both are parameters a person fills
 * in with the machine in front of them.
 *
 * Simpler than the edge's in every way that matters, and each simplification is a property of what
 * the agent *is* (ADR-0112: *"It holds nothing that matters. It decides nothing."*):
 *
 *  * **No update slots.** The agent is not over-the-air updatable; reinstalling it costs at most one
 *    duplicated ticket, which that record already accepts. `binPath` points at the binary directly.
 *  * **No database and no store key.** One small JSON file it rewrites through a rename.
 *  * **The same failure actions.** This half is not simplified, because it is the half that decides
 *    whether a terminal comes back after a crash — and a kitchen whose agent is dead prints nothing
 *    while the till keeps reporting `QUEUED_TO_AGENT`.
 *
 * @returns {string}
 */
export function printAgentInstallerTemplate() {
  const root = AGENT_WINDOWS_ROOT;
  return PS_BOM + [
    "# GENERATED FILE — do not edit.",
    "#",
    "# Emitted by dashboard/src/installers.mjs (printAgentInstallerTemplate) and regenerated by the",
    "# `dashboard` CI job, which fails if this file and that generator disagree. Edit the generator.",
    "#",
    "# Installs pos_print_agent on the terminal whose USB or serial port reaches a printer",
    "# (ADR-0112). Only stores whose edge runs somewhere other than the shop need one.",
    "",
    "#Requires -RunAsAdministrator",
    "<#",
    ".SYNOPSIS",
    "    pos_print_agent installer for one terminal.",
    "",
    ".DESCRIPTION",
    "    WHAT IT DOES, in order: creates the state directory, copies the binary, writes the config,",
    "    registers the service, puts the device token in the service's own environment, sets the",
    "    failure actions that bring the terminal back after a crash, and starts it. Idempotent:",
    "    running it twice is safe.",
    "",
    "    BEFORE RUNNING IT: pair this machine with the store's edge and have a manager bind it to the",
    "    terminal entry the console created. Until both are done the agent runs and is told, on every",
    "    claim, that it answers for no print agent.",
    "",
    "    -DeviceToken IS A SECRET. Prefer passing it interactively over leaving it in shell history.",
    "",
    ".PARAMETER Binary",
    "    Path to pos_print_agent.exe.",
    "",
    ".PARAMETER EdgeUrl",
    "    The store's edge, e.g. http://192.0.2.10:8787 on a shop LAN, or https://store.example.com",
    "    for an edge that runs somewhere else.",
    "",
    ".PARAMETER DeviceToken",
    "    The bearer token this machine was issued when it was paired with that edge.",
    "",
    ".PARAMETER Root",
    "    Where the agent's state lives.",
    "",
    ".EXAMPLE",
    "    powershell -ExecutionPolicy Bypass -File .\\install-pos-print-agent.ps1 `",
    "        -Binary .\\pos_print_agent.exe -EdgeUrl http://192.0.2.10:8787 -DeviceToken <token>",
    "#>",
    "[CmdletBinding()]",
    "param(",
    "    [Parameter(Mandatory = $true)]",
    "    [string] $Binary,",
    "",
    "    [Parameter(Mandatory = $true)]",
    "    [string] $EdgeUrl,",
    "",
    "    [Parameter(Mandatory = $true)]",
    "    [string] $DeviceToken,",
    "",
    `    [string] $Root = '${root}'`,
    ")",
    "",
    "Set-StrictMode -Version Latest",
    "$ErrorActionPreference = 'Stop'",
    "",
    "$service = 'pos-print-agent'",
    "",
    "if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {",
    "    throw \"no such binary: $Binary\"",
    "}",
    "$Binary = (Resolve-Path -LiteralPath $Binary).Path",
    "",
    "# No update slots: the agent is not over-the-air updatable, and reinstalling it costs at most one",
    "# duplicated ticket (ADR-0112). The service runs the copy under $Root, so replacing the agent is",
    "# re-running this script with a newer binary.",
    "New-Item -ItemType Directory -Force -Path $Root | Out-Null",
    "$installed = Join-Path $Root 'pos_print_agent.exe'",
    "$running = $null -ne (Get-Service -Name $service -ErrorAction SilentlyContinue)",
    "if ($running) { & sc.exe stop $service | Out-Null; Start-Sleep -Seconds 2 }",
    "Copy-Item -LiteralPath $Binary -Destination $installed -Force",
    "",
    "# The config. Readable by whoever is diagnosing a printer, and holding no credential.",
    "$config = @\"",
    printAgentToml({ edgeUrl: "$EdgeUrl", statePath: `${root}\\state.json` }),
    "\"@",
    "$configPath = Join-Path $Root 'print-agent.toml'",
    "# UTF-8 without a BOM: the TOML parser reads a leading BOM as part of the first key and refuses",
    "# the file. Set-Content -Encoding utf8 writes one on Windows PowerShell 5.1, which is what ships.",
    "[System.IO.File]::WriteAllText($configPath, $config, (New-Object System.Text.UTF8Encoding $false))",
    "",
    "if ($running) {",
    "    & sc.exe config $service binPath= \"`\"$installed`\"\" start= auto | Out-Null",
    "} else {",
    "    & sc.exe create $service binPath= \"`\"$installed`\"\" start= auto | Out-Null",
    "}",
    "& sc.exe description $service \"Pizza 4P's print agent (writes this terminal's printers)\" | Out-Null",
    "",
    "# The log file. Registered with sc.exe, this process has no console either, so without this its",
    "# standard output is discarded and the *silence reported twice* signal ADR-0112 leans on — the",
    "# till says QUEUED_TO_AGENT and the kitchen says nothing came — has no third place to look",
    "# (ADR-0117). The current run, plus the previous run beside it as pos-print-agent.log.1. It",
    "# carries no credential: the client and the configuration both redact the device token.",
    "$logPath = Join-Path $Root 'pos-print-agent.log'",
    "",
    "# Service-scoped rather than machine-wide, for the reason the edge's installer gives at length: a",
    "# machine environment variable is readable by every local administrator and shows up in process",
    "# listings of unrelated services. The device token belongs in neither place.",
    "$environment = @(",
    "    \"POS_PRINT_AGENT_CONFIG=$configPath\",",
    "    \"POS_PRINT_AGENT_TOKEN=$DeviceToken\",",
    "    \"POS_PRINT_AGENT_LOG_FILE=$logPath\",",
    "    'RUST_LOG=info'",
    ")",
    "$key = \"HKLM:\\SYSTEM\\CurrentControlSet\\Services\\$service\"",
    "New-ItemProperty -Path $key -Name 'Environment' -PropertyType MultiString -Value $environment -Force | Out-Null",
    "",
    "# NOT OPTIONAL, exactly as on the edge. SCM has no always-restart setting; without these actions a",
    "# crashed agent stays dead, the kitchen prints nothing, and the till keeps reporting",
    "# QUEUED_TO_AGENT because the queue is doing its job. reset= 86400 clears the failure count after",
    "# a day, so three bad days do not exhaust the list.",
    "& sc.exe failure $service reset= 86400 actions= restart/5000/restart/5000/restart/30000 | Out-Null",
    "",
    "& sc.exe start $service | Out-Null",
    "& sc.exe query $service",
    "Write-Host ''",
    "Write-Host 'pos_print_agent installed.'",
    "Write-Host 'Next: on the till, sign in as a manager and bind this device to its terminal entry.'",
    "Write-Host 'The token is now in the service registry key. Clear it from your shell history.'",
    "",
  ].join("\n");
}
