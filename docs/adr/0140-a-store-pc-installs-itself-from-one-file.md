# ADR-0140 — A store PC installs itself from one file

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-23
· Revisits roadmap-v3 **R3**, which chose a generated script over `pos-edge install` · Relates to
[ADR-0050](0050-activation-code-exchange.md), [ADR-0055](0055-edge-ota-updater.md),
[ADR-0117](0117-a-headless-store-keeps-a-log.md)

## The problem

A Windows store is brought online with two files and an elevated shell: the `pos_edge` binary, the
`install-pos-edge.ps1` the console generates, and a command line naming the binary — or, for a box the
console did not create, the parameterised script plus a store id and a cloud URL typed as parameters.
The script is right, and deliberately so: it lays out the update slots, writes `config.toml` without a
byte-order mark, opens the port on the Private profile, registers the service with the failure actions
that bring it back after an update, and prints the pairing URL. CI parses it under both PowerShell
editions.

What it is not is simple. A technician at a shop at 7 a.m. needs the right two files on a USB stick, an
administrator PowerShell, `-ExecutionPolicy Bypass`, and a runbook — and then, on a box with a cloud,
still has to find the activation screen. Roadmap-v3 R3 chose the generated script over
`pos-edge install --store <id>` because the wizard already held every value, and for the Linux script
that remains the right call. The step it left is the one the setup plan exists to remove: *download,
double-click, done.*

## Options considered

1. **Keep the script as the only path.** It works; it is also the part of bringing a store online that
   most often goes wrong in a way nobody sees until a till cannot connect.
2. **Rewrite the installer in Rust.** Service creation, failure actions, the service's registry
   environment and the firewall rule would all be re-implemented against Windows APIs — `pos-edge`
   forbids `unsafe`, so partly by shelling out anyway — and the tested script would be replaced by an
   untested one.
3. **Carry the script inside the binary, and let the binary work out the values.**

## Decision

**Option 3.** `pos-edge install` runs the embedded `install-pos-edge.ps1` — the same file the
console's generator emits and CI parses, compiled in with `include_str!` — with parameters it works out
for itself:

- **From its own file name.** A file named `pos-edge-setup_<cloud host>[@<port>]_<store ULID>.exe`
  installs when double-clicked, with no arguments: the name carries the store id and the cloud, which is
  all `config.toml` needs, and neither is a secret. A browser's `" (1)"` suffix is tolerated; the cloud
  is `https` except at `localhost` or a loopback address. The bytes are the release's own, so the
  minisign signature still verifies. Any other name — the installed `bin\current`, the rescue copy
  `pos-edge.exe` — runs as a server, exactly as before.
- **Or from a command line**: `pos-edge install --store <ULID> --cloud <https URL>`.
- **Elevation is asked for, not assumed.** A double-click is not elevated, so the first run starts the
  same file again through `Start-Process -Verb RunAs` — the UAC prompt — and returns.
- **The window stays open** (`-NoExit`), and the script's new `-OpenSetup` switch opens the box's
  `/setup` page when the service is up, because activation is the next step and the only one left.
- **Activation restarts the box.** The cloud loops start at boot behind the activation gate, so a box
  activated while it runs sat unsynced until somebody restarted it. A successful `POST /api/activate`
  now asks for the same graceful restart an installed update asks for, and `/setup` waits for the box to
  answer again before moving on to pairing.

## Consequences accepted

- **The secret still does not travel in the file.** The installed store has no scoped sync key until one
  is installed; activation delivers a device credential, which the `/sync` routes do not yet accept
  (ADR-0118). So a store installed this way sells, pairs and activates, and config sync and the order
  relay refuse until a key is put on the box — exactly the script's behaviour without `-SyncKey`. Having
  activation deliver what `/sync` needs is a port and wire change, and is left for its own record.
- **Nothing here hands out the tagged file yet.** A technician can rename a release's `pos-edge.exe`
  by hand; the console route that serves it already named is a separate change.
- **The script is compiled into every build**, a few kilobytes. It changes only through its generator,
  which the `dashboard` job diff-checks, so the embedded copy cannot drift from the one CI parses.
- **SmartScreen will warn on an unsigned file**, as it does today. Authenticode signing is a release
  decision with its own keys, not something this can supply.
- **Off Windows, `pos-edge install` says so and exits**: a Linux store runs the generated
  `install-pos-edge.sh`.

## Correction 1 — the name always means `https` (2026-09-23)

The decision above let a loopback host in the file name mean plain `http`. The edge's cloud transport
(`CloudHttpClient::new`) dials only `https`, loopback included, so such a name installed a box that
refused its own cloud and ran LAN-only. It was found by running the whole setup flow end to end
against a local cloud. A name now always means `https`, and `pos-edge install --cloud` refuses an
`http` URL wherever it points.

## Amendment 1 — a re-run carrying a newer release puts it in place (2026-09-25)

**Found on the owner's test PC.** Its old install ran 0.11.0. Re-running the 0.14.0 one-file
installer left it on 0.11.0, and the cloud said so. The script keeps `bin\current` on a re-run. That
is right for an **older** installer, which must never downgrade a shop that updated itself over the
air. It is wrong for a **newer** one, which a technician runs precisely to get the newer release.
0.11.0 then carried a till defect that 0.11.1 had fixed (a mistyped PIN unpaired the device), and the
PC kept it.

**Options considered.**
1. Keep the binary, and name both releases in the summary. The summary already does this; a
   technician still has to find the OTA screen for a release that is in their hand.
2. Overwrite a slot from PowerShell. That is a second copy of the swap logic, and it would lack the
   database backup and the boot trial that make an over-the-air install safe.
3. Let the carried binary put itself in place through the edge's own update steps.

**Decision: option 3.** Before it stops the service, the script asks `/healthz` which release this
store runs. It trusts only an answer for its own store. If `-CarriedVersion` is newer, it runs
`pos-edge.exe promote` from the rescue copy with the service stopped. `SystemdInstaller::promote`
takes the four steps an over-the-air install takes, in the same order: back up the database, stage,
self-test, commit. So `bin\previous`, the `.pre-update` copy and the unconfirmed marker are exactly
what an update leaves. The script ends the trial when the new release answers `/healthz`: the edge
clears the marker only once it reaches the cloud, and a box not yet activated would otherwise count
ordinary restarts against the new release until it reverted. A release that never answers is
replaced by the old one after three failed starts, as a bad update is.

A release that is older or equal is never put in place, and neither is one when the old process did
not answer: its release is unknown. The summary then says to run the installer again. The console's
downloadable script passes no release of its own, so it always keeps the binary. The Linux script is
unchanged.

**Consequences accepted.**
- `pos-edge` gains a `promote` subcommand. It is the script's to run, not a technician's, and it
  refuses a box with no `bin\current`.
- A failed pre-commit self-test now discards the staged bytes rather than reverting.
  `SystemdInstaller::rollback` did the revert in both cases. On a box that had updated before, that
  pointed `current` at the release *before* the running one, and copied the backup over the database
  the running store was still writing. On a first update it failed outright, leaving `staged` behind.
  The seam and its order are unchanged; `rollback` now undoes what the install did.
