# Bring a store online

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-25

Everything from an empty store machine to a till taking money, driven from the cloud dashboard — no
ULID typed, no command run on the server. This is the store-tier counterpart to
[Start from zero](start-from-zero.md), which brings up the cloud; do that first.

## Before you start

- The **cloud is deployed** and you are signed in as super-admin (Start from zero, Part 2).
- A **store machine** — a mini-PC in the shop, Windows or Linux — with the `pos_edge` binary on it.
- The machine and its devices (till, printer, KDS) share one LAN.

## The mental model — two tiers, three artefacts

The [two tiers](README.md#the-one-thing-to-understand-first-there-are-two-tiers) split cleanly: the
cloud is the back office, the store machine makes the money and never stops when the internet does.
Provisioning is the act of telling each side about the other. It produces **three artefacts**, and it
matters which is which:

| Artefact | What it is | Where it lives | Secret? |
|---|---|---|---|
| **`config.toml`** | Names *which store* this machine is (`store_id`) and *which cloud* it dials (`cloud_url`) | On the store machine, on disk | No — neither a store id nor a cloud URL is a secret ([ADR-0004](../adr/0004-cloud-owned-configuration.md)) |
| **`env`** | The store's scoped sync key, and the event-bus URL when there is one | `/etc/pos-edge/env`, root-owned, mode 0600 | **Yes** — this is the file that holds a credential |
| **Activation code** | A one-time `XXXX-XXXX-XXXX` a device trades for its credential | Handed to the device once, then spent | Treat as one — it *is* the credential until spent ([ADR-0050](../adr/0050-activation-code-exchange.md)) |
| **API key** (optional) | A token for the public `/v1` API | Your integration's secret store | Yes — shown once, never recoverable ([ADR-0037](../adr/0037-api-keys.md)) |

Each **device**'s credential is never in either file: it is minted by activation and kept in the
machine's OS keyring ([ADR-0051](../adr/0051-device-credential-provisioning.md)). That separation is
deliberate — a leaked `config.toml` cannot sell, and a machine swap re-activates without re-editing
anything. The **store**'s sync key is different: it authenticates the box itself to `/sync`, so it
lives in the keyring where possible and in the mode-0600 `env` file otherwise.

---

## Step 1 — Create the store in the dashboard

Pick the tenant in the top bar, then open the **Stores** screen and choose **Guided new store**
(`/stores/new`). The wizard, in three steps:

1. **Details** — name the store (e.g. *Bến Thành*) and, optionally, put it under a brand. It is created
   in the registry ([ADR-0065](../adr/0065-cloud-org-registry.md)); the ULID is assigned for you.
2. **API key** — issue the store's scoped key. `read_config` and `relay_orders` are pre-selected
   together and you should keep both: with only `read_config` the box syncs its configuration and
   looks healthy while the order relay answers `403` on every poll, so orders placed in the cloud
   never reach the kitchen. The key is shown **once** — the next step embeds it in a file for you.
3. **Handoff** — the wizard produces the two files the box needs, and an installer that contains
   both. Set the listen port here if this machine cannot use the default `8787`, then download what
   the next step calls for:
   - **`config.toml`** — `store_id`, `cloud_url`, and `bind` if you changed the port.
   - **`env`** — the sync key, plus a commented `POS_EDGE_NATS_URL`. Fill that line in when the
     cloud's event bus is open: `tls://:<token>@<your cloud host>:4222`, with the token from
     `deploy/secrets/nats.conf` on the VPS ([ADR-0089](../adr/0089-edge-event-bus-transport.md), and
     the *store event bus* section of the [deploy runbook](../deploy-runbook.md)). Without it the
     store still sells and still keeps every event durably in its outbox — but the cloud receives
     nothing, so rollups and reports read empty.
   - **`install-pos-edge.sh`** — a Linux installer for *this* store, carrying both files above
     inside it. Prefer it; Step 2 says why. It therefore **contains the store's key**: treat it as a
     password and delete it once the box is up.

> `config.toml` carries no secret, so it can sit beside the binary with ordinary permissions. `env`
> carries the one secret and must be installed root-owned and mode 0600. Do not merge them, and do not
> put the key in `config.toml` — the edge would load it, but the file is not protected like the other
> one, and a support screenshot of a config file should never leak a credential.
>
> The commented `advertised_ip` and `store_path` lines are optional overrides; leave them commented
> unless you have a reason. `tenant_id` is not a key the edge accepts — the store id is enough.

## Step 2 — Install the store server and drop the config

`pos_edge` runs as an operating-system service so it starts on boot and restarts on crash. On Linux,
let the wizard's installer do it; do it by hand on Windows, or on a host you manage some other way.

### The installer (Linux, recommended)

Copy the `pos_edge` binary, the unit file from
[`deploy/edge/pos-edge.service`](../../deploy/edge/pos-edge.service), and the downloaded
`install-pos-edge.sh` onto the machine, then:

```
sudo sh install-pos-edge.sh ./pos-edge ./pos-edge.service
```

It creates the `pos` service account, lays out the update slots, writes both files with the right
owners and modes, installs and enables the unit, and prints the status. Read it first if you like —
every line is plain `sh` and nothing in it reaches the network. Running it again is safe: it
refreshes the config, the unit and the rescue copy, and deliberately **leaves an already-installed
binary alone** so a box that has updated itself over the air is not quietly rolled back to whatever
binary you happened to be holding.

Then **delete the script** — it carries the store's key.

### By hand (Windows, or a host you manage yourself)

The step-by-step for both platforms is in
**[`deploy/edge/README.md`](../../deploy/edge/README.md)**. Put the downloaded `config.toml` where
`POS_EDGE_CONFIG` points (the systemd unit uses `/var/lib/pos-edge/config.toml`; the Windows example
uses `C:\pos\config.toml`), then install the `env` file with restricted permissions:

```
sudo install -o root -g root -m 0600 env /etc/pos-edge/env
```

The service unit reads it through `EnvironmentFile=-/etc/pos-edge/env` — the leading `-` means a
missing file is not an error, so a LAN-only demo box needs no env file at all. Then start the service.

One thing the manual path gets wrong more often than any other: since
[ADR-0055](../adr/0055-edge-ota-updater.md) Amendment 1 the unit starts
`/var/lib/pos-edge/bin/current`, a symlink the edge retargets to install its own updates. A box with
the binary only at `/usr/local/bin/pos-edge` trades perfectly well and silently never self-updates.
`deploy/edge/README.md` has the layout; the installer above exists because typing it is easy to get
wrong.

Either way, the machine now knows which store it is, opens its SQLite event log, and serves the store
UI on the LAN (`0.0.0.0:8787` by default).

## Step 3 — Start selling

Open the store UI from a device on the same LAN — scan the pairing QR the console prints, or type
`IP:8787` and the 6-digit pairing code ([ADR-0030](../adr/0030-pairing-and-offline-auth.md)). Sign a
cashier in with their PIN, open a table, ring up an item. **Unplug the network — it keeps working.**

This is the milestone that matters: the store can trade. Everything below connects it to the cloud, and
none of it is on the path of a sale.

## Step 4 — Name and activate the store's devices

Each till, printer, and kitchen display becomes a named device that trades on its own credential:

1. In the dashboard, **Activation** (with the store in context): pick the device by name, or **Add a
   device** (name + kind — POS terminal, printer, kitchen display, tablet). It is created in the
   registry — no ULID typed.
2. **Issue a code.** A `XXXX-XXXX-XXXX` activation code appears **once**. Give it to that device.
3. On the device, open the store's address in the browser. An unactivated box lands straight on
   **`/setup`**; type the code there. It exchanges the code with the cloud for a device credential,
   stores it in the OS keyring, and is activated from then on. A spent code is refused — one code,
   one device ([ADR-0050](../adr/0050-activation-code-exchange.md)). The screen folds the ambiguous
   glyphs (`I`/`L` → `1`, `O` → `0`) and groups the symbols as printed, so a typo is caught on the
   counter rather than after a round-trip.

> **Status today.** Activation and the cloud loops are composed into the shipping `pos_edge` binary
> (roadmap-v3 E1/E2/E3, [ADR-0086](../adr/0086-edge-keyvault-and-activation.md),
> [ADR-0087](../adr/0087-edge-relay-and-event-publish.md)): with `cloud_url` set in `config.toml`, the
> box serves `/setup` and `POST /api/activate`, stores the device credential in the OS keyring, and —
> once activated — pulls config, heartbeats, and pulls its cloud-placed orders automatically.
>
> The store's scoped key must carry **`relay_orders` as well as `read_config`**; with only the latter
> the relay is dark and the edge logs a `403` on every pull. Two flagged gaps remain: on a
> **headless Linux** box the kernel keyring is not durable across a reboot (the TPM-sealed hardening is
> the tracked hardware handoff), and the loops authenticate with that scoped key (the keyring's
> `sync_key`, or the `POS_EDGE_SYNC_KEY` env override) until the device credential is accepted on
> `/sync`. A store with no `cloud_url` still **sells fully offline from Step 3**.

## Step 5 — Publish the store's configuration

From **Configuration** (store in context), publish a config level — menu, tax, layout, capability flags.
The store pulls the new version over its sync channel and hot-reloads it, keeping the last-known-good if
a version is rejected ([ADR-0004](../adr/0004-cloud-owned-configuration.md)). Authoring the catalogue and
menu is its own workstream (roadmap Phase 2a); until then, publish a hand-written document here.

---

## The store is online

It trades offline, it is named in the registry, its devices are activated, and it takes configuration
from the cloud. To swap the machine later (the 5–10 minute "cattle, not pets" replacement), re-drop the
same `config.toml` and re-activate — the fresh box picks up where the old one left off
([ADR-0003](../adr/0003-cattle-not-pets.md)).

## Where to next

- **Connect a payment terminal, courier, or marketplace** → [Write an adapter](write-an-adapter.md).
- **Support a new country** (tax invoices, locale, local vendors) → [Add a country module](add-a-country-module.md).
- **Deploy or operate the cloud tier** → [deploy runbook](../deploy-runbook.md).
