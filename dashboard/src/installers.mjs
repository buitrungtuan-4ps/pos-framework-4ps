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
    "",
    "# The scoped store key (read_config + relay_orders). Shown once at issuance and not",
    "# recoverable — revoke and re-issue in the console if this file is lost.",
    v.key
      ? `POS_EDGE_SYNC_KEY=${v.key}`
      : "POS_EDGE_SYNC_KEY=  # issue a key in step 2, or paste one here",
    "",
    "# Where this store publishes its committed events. config.toml already names the stream and the",
    "# subject; this is the one part left, and it carries the broker token — which is why it is here.",
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
    "# broker refuses it. The token goes in the userinfo exactly as shown. Until this line is live the",
    "# edge logs that POS_EDGE_NATS_URL is unset and the outbox holds — the store trades either way.",
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
  return [
    "#!/bin/sh",
    "# pos_edge installer — generated by the new-store wizard for one specific store.",
    `# Store:  ${v.storeName}  (${v.storeId})`,
    `# Tenant: ${v.tenantLabel}  (${v.tenantId})`,
    "#",
    "# WHAT IT DOES, in order: creates the service user and the state directory, puts the binary in",
    "# the first update slot and points `current` at it, writes the bootstrap config and the",
    "# environment file (mode 0600, root-owned), installs the systemd unit, then enables and starts",
    "# the service. Idempotent: running it twice is safe and re-applies the same layout.",
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
    "systemctl daemon-reload",
    "systemctl enable --now pos-edge",
    "",
    "systemctl --no-pager --lines=0 status pos-edge || true",
    "echo",
    `echo "pos_edge installed for ${shDouble(v.storeName)} (${v.storeId})."`,
    `echo "Next: open http://<this box>:${v.bindPort.trim() || DEFAULT_BIND_PORT}/ on a device on the shop LAN and pair it."`,
    ...(v.key
      ? ['echo "Now DELETE this installer — it contains the store key."']
      : [
          'echo "WARNING: no key was issued, so /etc/pos-edge/env has no credential. Config sync and the order relay will not work until one is installed."',
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
 * @returns {string[]}
 */
function windowsBody({ configBlock, keyBlock, doneBlock, warningBlock, cloudHost, bindPort }) {
  return [
    "$service = 'pos-edge'",
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
    "if (Test-Path -LiteralPath $current) {",
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
    "# The environment, service-scoped rather than machine-wide. A machine environment variable is",
    "# readable by every local administrator and shows up in process listings of unrelated services;",
    "# this key is readable only by accounts that can read the service. REG_MULTI_SZ is how SCM passes",
    "# several variables to one service. SCM reads it at start, not on the fly, which is why the",
    "# restart below is not optional.",
    "$environment = @(",
    "    \"POS_EDGE_CONFIG=$configPath\",",
    "    'RUST_LOG=info'",
    ")",
    ...keyBlock,
    "",
    "# The event-bus URL is deliberately absent. Unlike the store key it is ONE secret shared by the",
    "# whole fleet, held on the cloud box, so the console cannot fill it in without spreading it across",
    "# every machine in the estate. Recover it on the cloud box, then add it here and restart:",
    "#",
    `#   $env = 'POS_EDGE_NATS_URL=tls://:<that token>@${cloudHost}:${NATS_CLIENT_PORT}'`,
    "#",
    "# Until it is set the edge logs that POS_EDGE_NATS_URL is unset and the outbox holds — the store",
    "# trades either way.",
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
    "if ($exists) { & sc.exe stop $service | Out-Null; Start-Sleep -Seconds 2 }",
    "& sc.exe start $service | Out-Null",
    "",
    "& sc.exe query $service",
    "Write-Host ''",
    ...doneBlock,
    // Double-quoted, because the parameterised variant passes a PowerShell variable here and a
    // single-quoted string would print the variable's name to the technician instead of the port.
    `Write-Host "Next: open http://<this box>:${bindPort}/ on a device on the shop LAN and pair it."`,
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
  return [
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
        "    slot and points `current` at it, writes the bootstrap config, registers the service with",
        "    the Service Control Manager, sets the service-scoped environment (including the store key),",
        "    sets the failure actions that are what bring the box back after an update, and starts it.",
        "    Idempotent: running it twice is safe and re-applies the same layout.",
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
            "# No key was issued with this store, so the service starts without a credential: it will",
            "# trade, and config sync and the order relay will keep refusing until one is installed.",
          ],
      doneBlock: [
        `Write-Host 'pos_edge installed for ${psSingle(v.storeName)} (${v.storeId}).'`,
      ],
      warningBlock: v.key
        ? ["Write-Host 'Now DELETE this installer — it contains the store key.'"]
        : [
            "Write-Host 'WARNING: no key was issued, so the service has no credential. Config sync and the order relay will not work until one is installed.'",
          ],
      cloudHost: v.cloudHost,
      bindPort: v.bindPort.trim() || DEFAULT_BIND_PORT,
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
  return [
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
        "    slot and points `current` at it, writes the bootstrap config, registers the service with",
        "    the Service Control Manager, sets the service-scoped environment (including the store key),",
        "    sets the failure actions that are what bring the box back after an update, and starts it.",
        "    Idempotent: running it twice is safe and re-applies the same layout.",
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
        "    The store's scoped API key (read_config + relay_orders). Omit to install without one:",
        "    the store trades, and config sync and the order relay refuse until a key is installed.",
        "",
        ".PARAMETER BindPort",
        `    The port the edge listens on. Defaults to ${DEFAULT_BIND_PORT}.`,
        "",
        ".PARAMETER Root",
        "    Where the store's state lives.",
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
        "# The scoped store key (read_config + relay_orders). The keyring is the better home for it",
        "# (ADR-0086) and this is the headless bring-up override, exactly as POS_EDGE_SYNC_KEY is on",
        "# Linux. Without one the store still trades; config sync and the order relay refuse.",
        "if ($SyncKey) {",
        "    $environment += \"POS_EDGE_SYNC_KEY=$SyncKey\"",
        "} else {",
        "    Write-Warning 'no -SyncKey given: config sync and the order relay will refuse until one is installed'",
        "}",
      ],
      doneBlock: ["Write-Host \"pos_edge installed for $StoreId.\""],
      warningBlock: [
        "if ($SyncKey) { Write-Host 'The store key is now in the service registry key. Clear it from your shell history.' }",
      ],
      cloudHost: "<your cloud host>",
      bindPort: "$BindPort",
    }),
  ].join("\n");
}
