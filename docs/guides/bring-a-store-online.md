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
| **`config.toml`** | Names *which store* this machine is (`store_id`) | On the store machine, on disk | No — a store id is not PII ([ADR-0004](../adr/0004-cloud-owned-configuration.md)) |
| **Activation code** | A one-time `XXXX-XXXX-XXXX` a device trades for its credential | Handed to the device once, then spent | Treat as one — it *is* the credential until spent ([ADR-0050](../adr/0050-activation-code-exchange.md)) |
| **API key** (optional) | A token for the public `/v1` API | Your integration's secret store | Yes — shown once, never recoverable ([ADR-0037](../adr/0037-api-keys.md)) |

The credential a device actually runs on is **never** in `config.toml`. The file carries identity; the
credential is minted by activation and kept in the machine's OS keyring
([ADR-0051](../adr/0051-device-credential-provisioning.md)). That separation is deliberate: a leaked
config file cannot sell, and a machine swap re-activates without re-editing any file.

---

## Step 1 — Create the store in the dashboard

Pick the tenant in the top bar, then open the **Stores** screen and choose **Guided new store**
(`/stores/new`). The wizard, in three steps:

1. **Details** — name the store (e.g. *Bến Thành*) and, optionally, put it under a brand. It is created
   in the registry ([ADR-0065](../adr/0065-cloud-org-registry.md)); the ULID is assigned for you.
2. **API key** — issue the scoped key the store's integrations use to reach the public API, if any. It
   is shown **once** — copy it now. Skip this if the store has no `/v1` integration yet; you can issue
   one later from the API keys screen.
3. **Handoff** — the wizard shows the store's `config.toml`, pre-filled with its `store_id` and, as
   comments, the store and tenant names and this cloud's URL. **Download it** (or Copy). This is the
   only file you carry to the machine.

> The generated file has exactly one active line — `store_id = "…"`. The commented `bind`,
> `advertised_ip`, and `store_path` lines are optional overrides; leave them commented unless you have
> a reason. The edge rejects any key it does not recognise, so do not add `tenant_id`, a cloud URL, or
> the API key to this file — none of them belong there.

## Step 2 — Install the store server and drop the config

Install `pos_edge` as an operating-system service so it starts on boot and restarts on crash — the
step-by-step for systemd and Windows is in **[`deploy/edge/README.md`](../../deploy/edge/README.md)**.
Put the downloaded `config.toml` where `POS_EDGE_CONFIG` points (the systemd unit uses
`/var/lib/pos-edge/config.toml`; the Windows example uses `C:\pos\config.toml`), then start the service.

The machine now knows which store it is, opens its SQLite event log, and serves the store UI on the LAN
(`0.0.0.0:8787` by default).

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
