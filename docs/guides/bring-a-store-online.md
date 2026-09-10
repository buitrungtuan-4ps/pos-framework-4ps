# Bring a store online

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-25

Everything from an empty store machine to a till taking money, driven from the cloud dashboard — no
ULID typed, no command run on the server. This is the store-tier counterpart to
[Start from zero](start-from-zero.md), which brings up the cloud; do that first.

## Before you start

- The **cloud is deployed** and you are signed in as super-admin (Start from zero, Part 2).
- A **store machine** — a mini-PC in the shop, Windows or Linux — with the `pos_edge` binary on it.
- The machine and its devices (till, printer, KDS) share one LAN.

The five steps are in the order they have to happen: **create · install · activate · publish · sell.**
Each one is a precondition of the next, and none of them can be skipped or reordered — an
unactivated box serves nothing but `/setup`, and a box with no published configuration has no staff
roster to sign anyone in against. [`docs/go-live.md`](../go-live.md) is the same sequence for a whole
estate.

## The mental model — two tiers, three artefacts

The [two tiers](README.md#the-one-thing-to-understand-first-there-are-two-tiers) split cleanly: the
cloud is the back office, the store machine makes the money and never stops when the internet does.
Provisioning is the act of telling each side about the other. It produces **three artefacts**, and it
matters which is which:

| Artefact | What it is | Where it lives | Secret? |
|---|---|---|---|
| **`config.toml`** | Names *which store* this machine is (`store_id`), *which cloud* it dials (`cloud_url`), and *which event stream* it publishes into (`[nats]`) | On the store machine, on disk | No — a store id, a cloud URL and a stream name are all public facts ([ADR-0004](../adr/0004-cloud-owned-configuration.md)) |
| **`env`** | The store's scoped sync key, and the event-bus URL when there is one | `/etc/pos-edge/env`, root-owned, mode 0600 | **Yes** — this is the file that holds a credential |
| **Activation code** | A one-time `XXXX-XXXX-XXXX` the **store server** trades for its cloud credential — one per box, not one per till | Typed once at `/setup`, then spent | Treat as one — it *is* the credential until spent ([ADR-0050](../adr/0050-activation-code-exchange.md)) |
| **API key** (optional) | A token for the public `/v1` API | Your integration's secret store | Yes — shown once, never recoverable ([ADR-0037](../adr/0037-api-keys.md)) |

The box's credential is never in either file: it is minted by activation and kept in the machine's OS
keyring ([ADR-0051](../adr/0051-device-credential-provisioning.md)). That separation is deliberate — a
leaked `config.toml` cannot sell, and a machine swap re-activates without re-editing anything. The
**store**'s sync key is different: it authenticates the box itself to `/sync`, so it lives in the
keyring where possible and in the mode-0600 `env` file otherwise.

The tills are a **third** thing, and the one most often confused with activation. A tablet does not
activate; it **pairs**, with a six-digit code the store server itself mints and holds in memory for
five minutes ([ADR-0030](../adr/0030-pairing-and-offline-auth.md)). Activation is the box telling the
cloud who it is; pairing is the box telling a tablet it may talk to it. They involve different codes,
different lifetimes and different sides of the wire, and the steps below do them in that order.

---

## Step 1 — Create the store in the dashboard

Pick the tenant in the top bar, then open the **Stores** screen and choose **Guided new store**
(`/stores/new`). The wizard, in three steps:

1. **Details** — name the store (e.g. *Bến Thành*) and, optionally, put it under a brand. It is created
   in the registry ([ADR-0065](../adr/0065-cloud-org-registry.md)); the ULID is assigned for you.
2. **API key** — issue the store's scoped key. It is issued **bound to the store you just created**,
   which is what the `/sync/stores/{id}/…` routes require: those serve one store its own
   configuration, employee roster included, so a key naming another store — or naming none — is
   refused there. `read_config` and `relay_orders` are pre-selected together and you should keep
   both: with only `read_config` the box syncs its configuration and looks healthy while the order
   relay answers `403` on every poll, so orders placed in the cloud never reach the kitchen. The key
   is shown **once** — the next step embeds it in a file for you.

   Issuing a store key by hand instead (**API keys** screen) works the same way, but you must pick
   the store in *Which store is this key for?*. A tenant-wide key is right for an integration that
   reads a whole tenant's rollups and wrong for a box: the box will authenticate and then be refused
   on every sync call.
3. **Handoff** — the wizard produces the two files the box needs, and an installer that contains
   both. Set the listen port here if this machine cannot use the default `8787`, then download what
   the next step calls for:
   - **`config.toml`** — `store_id`, `cloud_url`, `bind` if you changed the port, and the `[nats]`
     stream and subject the store publishes its committed events into. Those last two are the
     **fleet's**, not this store's, and they match the `[nats]` section of `cloud.toml` on the cloud
     box — one stream, one subject, the same on every store ([ADR-0087](../adr/0087-edge-relay-and-event-publish.md)
     Amendment 1). Leave them alone unless you have changed the cloud's side too.
   - **`env`** — the sync key, plus a commented `POS_EDGE_NATS_URL`. Fill that line in when the
     cloud's event bus is open: `tls://:<token>@<your cloud host>:4222`, with the token from
     `deploy/secrets/nats.conf` on the VPS ([ADR-0089](../adr/0089-edge-event-bus-transport.md), and
     the *store event bus* section of the [deploy runbook](../deploy-runbook.md)). Without it the
     store still sells and still keeps every event durably in its outbox — but the cloud receives
     nothing, so rollups and reports read empty.
   - **`install-pos-edge.sh`** — a Linux installer for *this* store, carrying both files above
     inside it. Prefer it; Step 2 says why. It therefore **contains the store's key**: treat it as a
     password and delete it once the box is up.
   - **`install-pos-edge.ps1`** — the same for a Windows till, carrying `config.toml` inside it and
     putting the key on the service's own registry key (Windows has no `env` file). Same warning:
     it contains the store's key.

> `config.toml` carries no secret, so it can sit beside the binary with ordinary permissions. `env`
> carries the one secret and must be installed root-owned and mode 0600. Do not merge them, and do not
> put the key in `config.toml` — the edge would load it, but the file is not protected like the other
> one, and a support screenshot of a config file should never leak a credential.
>
> The commented `advertised_ip` and `store_path` lines are optional overrides; leave them commented
> unless you have a reason. `tenant_id` is not a key the edge accepts — the store id is enough.

## Step 2 — Install the store server and drop the config

`pos_edge` runs as an operating-system service so it starts on boot and restarts on crash. On both
Linux and Windows, let the wizard's installer do it; go by hand only on a host you manage some other
way.

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

### The installer (Windows, recommended)

Copy the `pos_edge` binary and the downloaded `install-pos-edge.ps1` onto the machine, then, in a
PowerShell started as an administrator:

```
powershell -ExecutionPolicy Bypass -File .\install-pos-edge.ps1 -Binary .\pos-edge.exe
```

It lays out the same update slots, writes `config.toml` with an **absolute** database path, registers
the service on `bin\current`, puts the store key on the service's own registry key rather than in a
machine-wide variable, and sets the failure actions that are what bring the box back after an
over-the-air update. Running it again is safe, and it leaves an already-installed binary alone for
the same reason the Linux one does.

Then **delete the script** — it carries the store's key.

If you have no wizard-generated copy — a store the console did not create — use
[`deploy/edge/install-pos-edge.ps1`](../../deploy/edge/install-pos-edge.ps1), which is the same
script taking the store's values as parameters.

### By hand (a host you manage yourself)

The step-by-step for both platforms is in
**[`deploy/edge/README.md`](../../deploy/edge/README.md)**. Put the downloaded `config.toml` where
`POS_EDGE_CONFIG` points (the systemd unit uses `/var/lib/pos-edge/config.toml`; the Windows example
uses `C:\pos\config.toml`), then install the `env` file with restricted permissions:

```
sudo install -o root -g root -m 0600 env /etc/pos-edge/env
```

The service unit reads it through `EnvironmentFile=-/etc/pos-edge/env` — the leading `-` means a
missing file is not an error, so a LAN-only demo box needs no env file at all. Then start the service.

**On Windows there is no `env` file** and the command above is POSIX-only. The two values it carries
— `POS_EDGE_SYNC_KEY` and `POS_EDGE_NATS_URL` — go on the service's own registry key instead (which
is what the installer above does for you);
[`deploy/edge/README.md`](../../deploy/edge/README.md) has the exact `reg add` line and why
service-scoped beats machine-wide. The sync key's proper home on either OS is the credential store,
and the environment variable is a headless bring-up override.

One thing the manual path gets wrong more often than any other: since
[ADR-0055](../adr/0055-edge-ota-updater.md) Amendment 1 the unit starts
`/var/lib/pos-edge/bin/current`, a symlink the edge retargets to install its own updates. A box with
the binary only at `/usr/local/bin/pos-edge` trades perfectly well and silently never self-updates.
`deploy/edge/README.md` has the layout; the installer above exists because typing it is easy to get
wrong.

Either way, the machine now knows which store it is, opens its SQLite event log, and serves the store
UI on the LAN (`0.0.0.0:8787` by default).

### The listen port, which on Windows is not open by default

**The installers open it for you** — this used to be a step here, and the failure it causes is the
most misleading one in the whole bring-up, so it moved into the script.

Defender Firewall drops an inbound connection to a port no rule names, silently and with no log line
on either side. A Windows store installed correctly in every other respect therefore comes up, opens
its database and writes its pairing URL, while every till on the floor gets a connection timeout —
which looks exactly like a broken install. The Windows installer adds one rule, `pos-edge (TCP
8787)`, on the **Private** profile only: the port is plain HTTP on the shop LAN, so a Public-profile
rule would offer the till API to whatever network the box is plugged into next. Domain is left alone
deliberately, for an estate that manages this with group policy. Re-running with a different port
moves the rule rather than leaving the old port open beside the new one.

On Linux there is usually nothing to do — a stock Debian or Ubuntu box ships no active packet filter
— so the installer acts only on a filter that is actually **running** (`ufw status` reporting active,
or `firewall-cmd --state` succeeding). A blind `ufw allow` on a box where ufw is installed but
inactive would silently record a rule that first takes effect the day somebody enables the firewall
for an unrelated reason, which is a change to the store's exposure made months earlier by an
installer nobody remembers running.

If the rule could not be added — a firewall switched off, driven by policy, or a Windows build
without the `NetSecurity` module — the installer says so and carries on: the service is registered
and running either way, and it is the port, not the store, that needs attention. Verify from a
second machine on the LAN with `curl http://<the box>:8787/` before you go looking for anything else.

None of this is a perimeter rule. **The store still accepts no inbound connection from the cloud**
and needs no port forward on the router: every cloud→store flow is a store-initiated pull
([ADR-0001](../adr/0001-offline-first-store-autonomy.md),
[ADR-0061](../adr/0061-order-relay.md)). What is being opened is one host firewall, to the tills and
kitchen displays on the same LAN.

### Reading the boot log

**Do this now, before Step 3.** Everything the store server has to say — a config it will not parse, a
missing font, a refused sync, and the pairing URL Step 5 needs — is in its log, and each of the
remaining steps sends you back here when it does not behave.

* **Linux** — `journalctl -u pos-edge -n 50`. `systemd` captures the process's output by default; the
  unit configures no log file, and needs none.
* **Windows** — `Get-Content C:\ProgramData\pos-edge\pos-edge.log -Tail 50`. A service started by the
  Service Control Manager has **no console**, so that output goes nowhere unless the server is pointed
  at a file — which is what the installer's `POS_EDGE_LOG_FILE` does
  ([ADR-0117](../adr/0117-a-headless-store-keeps-a-log.md)). The file is rewritten at every start-up,
  keeping the previous run beside it as `pos-edge.log.1`, and it is capped so it cannot fill the disk
  `store.sqlite` lives on. `-Tail 50` rather than Notepad: it is the bounded read `journalctl -n 50`
  is, and `tracing` writes LF-only lines that older Notepad renders as one.

The line to look for is `pos_edge listening`. If it is not there, nothing after it happened either.

## Step 3 — Activate the store

Activation is what gives the box its cloud identity, and it is **ahead of pairing and ahead of
selling**: until it is done the store serves nothing but `/setup`, and no cloud loop runs
([ADR-0086](../adr/0086-edge-keyvault-and-activation.md)).

1. In the dashboard, **Activation** (with the store in context): pick the device by name, or **Add a
   device** (name + kind — POS terminal, printer, kitchen display, tablet). It is created in the
   registry — no ULID typed.
2. **Issue a code.** A `XXXX-XXXX-XXXX` activation code appears **once**.
3. On any device on the store's LAN, open the store server's address in a browser. An unactivated box
   lands straight on **`/setup`**; type the code there. The **store server** — not the browser —
   exchanges it with the cloud for a credential and keeps it in its own OS keyring. From then on the
   store is activated and its config, heartbeat and order-relay loops run. A spent code is refused
   ([ADR-0050](../adr/0050-activation-code-exchange.md)). The screen folds the ambiguous glyphs
   (`I`/`L` → `1`, `O` → `0`) and groups the symbols as printed, so a typo is caught on the counter
   rather than after a round-trip.

> **One credential per box, not one per device.** This is the thing to get right, because the console
> will happily issue a code per named device and only the first one typed can ever be redeemed. The box
> holds a single `DeviceCredential`; a second code presented to an already-activated store is refused
> `409`. So issue **one** code, for the store server, and let the named printer and kitchen-display
> rows in the registry be what they are — inventory, not credential holders. Per-device cloud
> credentials are a recorded, deferred end state
> ([ADR-0041](../adr/0041-device-onboarding.md)), not what this release does.
>
> `-SyncKey` does **not** substitute for this. The scoped store key authenticates the box to `/sync`;
> the boot gate reads the *device credential*, and with no credential the loops are not spawned at all.

> **Status today.** Activation and the cloud loops are composed into the shipping `pos_edge` binary
> (roadmap-v3 E1/E2/E3, [ADR-0086](../adr/0086-edge-keyvault-and-activation.md),
> [ADR-0087](../adr/0087-edge-relay-and-event-publish.md)): with `cloud_url` set in `config.toml`, the
> box serves `/setup` and `POST /api/activate`, stores the credential in the OS keyring, and — once
> activated — pulls config, heartbeats, and pulls its cloud-placed orders automatically.
>
> The store's scoped key must carry **`relay_orders` as well as `read_config`**; with only the latter
> the relay is dark and the edge logs a `403` on every pull — which is exactly the symptom *Reading the
> boot log* above exists for, because the box otherwise looks healthy. One flagged gap remains: on a
> **headless Linux** box the kernel keyring is not durable across a reboot, and the TPM-sealed
> hardening is a tracked hardware handoff ([`gate-register.md`](../gate-register.md) row P2).

## Step 4 — Publish the store's configuration

From **Configuration** (store in context), publish a config level — menu, tax, layout, capability
flags, and **permissions**, which is the staff roster the store authorises sign-ins against. The store
pulls the new version over its sync channel and hot-reloads it, keeping the last-known-good if a
version is rejected ([ADR-0004](../adr/0004-cloud-owned-configuration.md)). Authoring the catalogue and
menu is its own workstream (roadmap Phase 2a); until then, publish a hand-written document here.

**The permissions node is not optional and neither is the menu.** A freshly installed store boots with
an *empty* roster and an *empty* catalogue. Without the permissions publish, every sign-in answers the
same `401` as a wrong PIN — there is no roster to check the PIN against — and without a menu there is
nothing to price. That is why this step is ahead of Step 5 rather than after it: a reader who tries to
sell first finds a till that refuses every cashier and no error that says why.

Watch the box take it: *Reading the boot log* above shows the pull. The interval is 30 seconds, so a
publish reaches a healthy store inside a minute.

## Step 5 — Start selling

Open the store UI from a device on the same LAN. It needs the pairing URL, and on a store server there
is no screen to read it off — so read it from the box:

* **Windows** — `Get-Content C:\ProgramData\pos-edge\pairing-url.txt`. The Windows installer also
  prints it before it exits.
* **Linux** — `journalctl -u pos-edge | grep pair`.

Either way it is `http://<ip>:8787/pair?code=NNNNNN` (or `https://<host>/pair?code=NNNNNN` on a store
given a public origin, [ADR-0111](../adr/0111-a-second-origin-may-address-the-edge.md)). Scan it, or
type `IP:8787` and the six digits ([ADR-0030](../adr/0030-pairing-and-offline-auth.md),
[ADR-0117](../adr/0117-a-headless-store-keeps-a-log.md)). Then sign a cashier in with their PIN, open
a table, ring up an item. **Unplug the network — it keeps working.**

> **The boot code pairs the first device. After that, mint from inside the store.**
>
> The boot code is minted once when the process starts, lives **five minutes**, and is single-use;
> redeeming it deletes the file. For the **second and every later** device you do not restart
> anything: on a till that is already paired, with a **manager signed in**, open **Devices** in the
> top bar and choose *Get a pairing code*. The six digits appear on screen, along with the URL to
> open on the new tablet ([ADR-0118](../adr/0118-one-credential-per-box-and-the-cloud-learns.md)).
>
> That code replaces whatever was live, including the boot one — the store never holds two at once.
> It needs the `ManageDevices` permission, so a waiter's tap cannot admit hardware; if the button
> answers *"needs a manager signed in on this device"*, that is standing missing, not a broken
> pairing.
>
> **Restarting still works and is still the fallback** — for a store with no till paired yet, or one
> whose only manager is not there. It costs every till's and kitchen display's live session and
> forces the outbox to drain, so do it outside service hours. On Windows: `sc.exe stop pos-edge` then
> `sc.exe start pos-edge`. On Linux: `sudo systemctl restart pos-edge`.

This is the milestone that matters: the store can trade, and from here it trades whether or not the
cloud is reachable. What it needed first was a published configuration — the roster it authorises
sign-ins against and the menu it prices from both arrive in Step 4, and are empty until then.

---

## Retiring a till that was lost or replaced

A tablet pairs once and stays paired: pairings are durable by design
([ADR-0091](../adr/0091-durable-edge-auth-state.md)), so nothing expires them and a restart no longer
unpairs the store. That is the right default for a shop that reboots mid-service, and it means a
tablet that walks out of the building keeps working until somebody retires it.

On any **paired** device in the store, open **Devices** in the top bar:

1. Every device the store has admitted is listed, newest first, with the moment it paired. The
   tablet you are holding is marked **This device** — that is the one row not to retire by accident.
2. **Retire** the row that is gone, then confirm. Its token stops resolving at once and does not come
   back after a restart. Everyone else keeps trading.
3. If you cannot tell which row is the missing tablet, **Retire every device** is the break-glass, and
   it is still the most expensive thing on this screen: type `ALL` to confirm, and **every till stops
   working at the same instant** — including the one you are holding, which signs you out. Recovering
   means pairing the first till from the boot code (so: restart the store server), and only then can
   *Get a pairing code* on that till admit the rest. Retire one row at a time wherever you can tell
   which one it is; save `ALL` for the case where you genuinely cannot.

The edge does not know a device's *name* — device names live in the cloud's approved-device registry,
and a store that has never synced has none — so the pairing moment and the **This device** mark are
what tell the tills apart. Retiring is behind the paired-device gate, not an operator login: the
store server has no operator identity offline (the console is a browser on the LAN), so it is as
strong as pairing and no stronger, and every retirement is written to the store's log.

If a retirement answers an error, the store server could not write its durable device table — the
tablet may still be paired after a restart. Try again rather than assuming it is locked out.

---

## Replacing the machine

A store server is cattle, not a pet ([ADR-0003](../adr/0003-cattle-not-pets.md)): the shop's identity
lives in the cloud, so replacing the box is a re-install and not a recovery. What follows is the same
procedure whether the machine died overnight or is being retired on schedule.

**Start in the console: Stores → the store's row → "Move to a new box".** That drawer is where the
files come from, and it exists because they used to come only from the new-store wizard — which
would have meant creating a *second* store to get an installer for an existing one. It leads with
what a dead machine takes with it, because two of the four things are not in any file:

| | Recovery |
| --- | --- |
| Which store this is, and which cloud it dials | **Regenerated.** A pure function of the registry row and this cloud's own origin. |
| The store's sync key (`POS_EDGE_SYNC_KEY`) | **Re-issue.** Shown once and stored hashed, so it cannot be read back. The drawer issues a fresh one, scoped to the store with `read_config` + `relay_orders`. |
| The device credential activation minted | **Gone.** It never left the old machine — it is in *that* machine's OS keyring ([ADR-0086](../adr/0086-edge-keyvault-and-activation.md)), not in a file and not on any screen. |
| The store's event log (`store.sqlite`) | **Gone, in part.** Everything already published is safe in the fleet stream; anything still in the outbox is not. |

Then, on the new machine:

1. **Run the installer** the drawer gave you, with the binary. Same script as a first install and
   safe to run twice.
2. **Read the boot log** and find `pos_edge listening` (§"Reading the boot log").
3. **Activate it.** This is the step that is easiest to miss, and the reason the drawer leads with
   prose: a replacement box installs cleanly, boots, serves `/setup` — and will not sell, because
   the boot gate reads a device credential it does not have. Issue a new activation code on
   **Activation** and type it on the new box's `/setup`. A fresh code can always be minted for a
   device slot; the `409` you may have read about is the *edge* refusing a second code for a box
   that is already activated, which a new machine is not.
4. **Pair the tills.** The installer prints the pairing URL; the log holds it too.
5. **Take the old machine off the network, and revoke its key** on **API keys**. Issuing a new key
   does not disable the old one. Since
   [ADR-0123](../adr/0123-a-superseded-box-opens-nothing-new.md) the bumped lease does stop the old
   box **opening** anything new — no new table, no new counter order, no new shift, and no channel
   order from the relay — but it deliberately lets the tables it already holds finish and settle, so
   nobody's dinner is stranded on it. It is still not a substitute for unplugging the thing: while
   it is powered on it is still finishing orders and still minting receipt numbers. Unplug or wipe
   the one you replaced; do not leave it running "just in case".

**Do the bump before you activate the replacement.** In that order the new box takes the store's
current generation on first sight and is the store. The other way round it takes the old generation,
and the bump then supersedes the machine that is actually trading.

If the old disk is readable, copy `store.sqlite` across before starting the new server: that is the
only way to recover events the box committed but had not yet published. There is no off-box backup
of it in this release — [ADR-0046](../adr/0046-backups-and-restore.md)'s store half (edge WAL
shipping) is unbuilt, and the gate register records that rather than hiding it. Copying it is safe
for the lease: **activation forgets any lease generation the copied database carried**, so the
replacement does not inherit the dead box's identity and refuse to seat a table (ADR-0123). That is
also why step 3 is not optional on a box built from a copied disk — without the activation, the
inherited generation stands.

## The store is online

It trades offline, it is named in the registry, it is activated, its tills are paired, and it takes
configuration from the cloud.

## Where to next

- **Connect a payment terminal, courier, or marketplace** → [Write an adapter](write-an-adapter.md).
- **Support a new country** (tax invoices, locale, local vendors) → [Add a country module](add-a-country-module.md).
- **Deploy or operate the cloud tier** → [deploy runbook](../deploy-runbook.md).
