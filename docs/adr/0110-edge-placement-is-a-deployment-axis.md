# ADR-0110 — Edge placement is a deployment axis, and the lease is what moves along it

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Bounds [ADR-0001](0001-offline-first-store-autonomy.md)'s offline guarantee to one placement
· Rests on [ADR-0049](0049-single-active-lease.md) and
[ADR-0108](0108-the-lease-generation-is-authority.md) for mutual exclusion
· Keeps [ADR-0002](0002-one-binary-per-tier.md) and re-reads
[ADR-0003](0003-cattle-not-pets.md)'s replacement as a move, not only a swap
· Relates to [ADR-0068](0068-fleet-liveness.md), [ADR-0085](0085-edge-cloud-sync-transport.md),
[ADR-0061](0061-order-relay.md), [ADR-0062](0062-the-relay-wake.md)

This is the first of five records on the same programme. The other four are **ADR-0111** (a second
origin may address the edge), **ADR-0112** (the print agent), **ADR-0113** (the host tier and the
console's Start button) and **ADR-0114** (region as a required, recorded attribute). Those numbers
are reserved and those files do not exist yet, so they are named here in plain text and not linked —
`xtask links` fails a build on a link that does not resolve, and a reserved number is not a document.

## The problem

### The edge does not need the shop; the deployment assumes it anyway

Every connection `pos_edge` opens is outbound, without exception. Activation goes out
([`activation.rs`](../../crates/pos-edge/src/activation.rs) over
[`cloud_http.rs`](../../crates/pos-edge/src/cloud_http.rs)). The config pull and the heartbeat go out
([`heartbeat_client.rs`](../../crates/pos-edge/src/heartbeat_client.rs)). The updater goes out
([`ota_client.rs`](../../crates/pos-edge/src/ota_client.rs)). Events go out
([`event_publish.rs`](../../crates/pos-edge/src/event_publish.rs)). Cloud-to-store work goes out too,
because [ADR-0061](0061-order-relay.md) made it a pull the store initiates and
[ADR-0062](0062-the-relay-wake.md) refused to make it anything else. [`AGENTS.md`](../../AGENTS.md)
§1 states it as a rule of the system: *"Stores only make outbound connections. The cloud never dials
into a store."*

Nothing in that list cares which building the process is in. `cloud_url` in
[`config.rs`](../../crates/pos-edge/src/config.rs) is a URL; the edge dials it from wherever it is.

The assumption lives everywhere *else*. [`deploy/edge/`](../../deploy/edge/) holds a systemd unit and
a PowerShell installer for a machine somebody carries into a shop, and
[`deploy/Dockerfile`](../../deploy/Dockerfile) builds only `pos-cloud` — there is no edge image.
[`discovery.rs`](../../crates/pos-edge/src/discovery.rs) is documented as *"advertising the edge on
the store LAN"*. `EdgeConfig::advertised_ip` is documented as *"The LAN IP to put in the pairing URL
…, pinned by a DHCP reservation"*. [`docs/glossary.md`](../glossary.md) defines the term itself as
**"the in-store runtime (`pos_edge`) and the machine it runs on"**.

So the constraint is not in the protocol, the schema or the domain. It is in the installer, the
pairing URL and the vocabulary.

### The offline promise is written as a property of the system, so no store may lack it

[ADR-0001](0001-offline-first-store-autonomy.md) says all logic required to take an order, route it
to the kitchen, take payment and print a receipt runs inside the store against local SQLite.
`AGENTS.md` §1 says *"It works with no internet."* Both are statements about **the system**, and
both are load-bearing — ADR-0001 is the record every other record defers to when it asks "what
happens offline?".

That leaves no honest way to describe a store whose edge is somewhere else. Such a store does not
satisfy ADR-0001, and there is no field, word or screen that can say so. The choice today is to
either not do it, or to do it and quietly make a documented system property false for an unknown
subset of the fleet. Neither is acceptable at the size this framework is aimed at.

### The counter-examples are ordinary, not exotic

At 500+ stores, 10+ brands and around five countries, two situations stop being edge cases.

Some sites cannot host reliable hardware. A mall kiosk with no lockable cupboard, a twelve-square-
metre takeaway counter, a site whose power and network belong to the landlord: `docs/architecture.md`
asks for an x86-64 box with an SSD that is *never an SD card*, and some sites will not get one and
keep it healthy.

Some operators will not accept a machine they do not control. They run infrastructure already, they
have a change process, and they would rather run the process themselves than have a mini-PC appear
in their shop. The framework is forked and self-hosted by design
([ADR-0044](0044-fork-and-deploy.md)); an operator who already runs `pos_cloud` asking to also run
`pos_edge` is asking for something the code already supports.

### There is no word for it and no rule for what changes

Without a name there is no diff. Nobody can answer "what does this store lose, and what does the
console show differently?" except by reasoning it out again in each conversation. Four other records
are about to depend on that answer, so it has to be settled once, here, before any of them is
written.

## The decision

**Placement is an attribute of a store with exactly three values — `in-store`,
`hosted-by-operator`, `hosted-by-platform` — that changes where `pos_edge` runs and nothing else
about what it is.**

### The three modes, defined by the network and by who is accountable

**`in-store`.** The edge runs on a machine on the shop LAN. The selling devices reach it over that
LAN. It sells with the cable to the internet unplugged. This is every store today, and it stays the
default.

**`hosted-by-operator`.** The same binary runs on a machine the operator owns — a VPS, a rack in
their office, a box in a regional hub. The platform does not provision it, does not have a shell on
it, and learns about it exactly the way it learns about any store: activation, heartbeat, config
pull, events.

**`hosted-by-platform`.** An admin picks a region in the console and presses Start, and the platform
stands the edge up. That is ADR-0113's job to build; this record's job is to say the mode exists and
is real, so nothing downstream is designed as though the console button were hypothetical.

The line between modes 2 and 3 is **who owns the machine and who is accountable when it stops** —
not technology. Mode 2 and mode 3 run identical software over identical connections. The line that
changes *behaviour* is the one between mode 1 and modes 2/3: whether the selling devices and the edge
are on the same LAN.

The framework stays neutral on where an operator hosts. It does not prefer mode 3, does not price
mode 2 out, and does not hide either. What it insists on is that the choice is **recorded and
visible**, because it changes what a store can do.

### The domain does not change, and that is the entire reason this axis is affordable

One binary. One SQLite schema and one migration series. One lease. One API key on `/sync`
([ADR-0085](0085-edge-cloud-sync-transport.md)). One outbound sync. One `pos-core`.

An order, a bill, a tax computation ([ADR-0104](0104-multi-component-and-inclusive-tax.md)), a
receipt number ([ADR-0025](0025-receipt-number-authority.md)), a stock decrement at fire
([ADR-0079](0079-inventory-and-suppliers.md)) and a rendered kitchen ticket
([ADR-0100](0100-receipt-and-ticket-printing.md)) are all computed by the same code against the same
rows, whichever mode the store is in. Placement is invisible to every one of them.

This works because the hard part is already done. A framework whose store dialled *in* would have to
invert its transport to be hosted; this one has nothing to invert, because `AGENTS.md` §1's outbound
rule made every path a hosted placement needs the only path that exists. The axis is cheap for
exactly one reason: it was accidentally paid for years ago.

Keeping it cheap is a constraint on everything after this record. **The moment a domain path branches
on placement, the framework has two point-of-sale systems and tests one of them.** Placement may
change *where a byte is written* — the printer's last hop, the origin a browser talks to — and may
never change what the byte says. Those two exceptions are ADR-0112 and ADR-0111, and both are at the
outermost edge of the system, on purpose.

### ADR-0001's guarantee belongs to `in-store` only, and the console must say so

Plainly: **a hosted store with no internet cannot sell.** The till is a browser on the counter, the
edge is across a WAN, and when the WAN is down the browser has nothing to talk to. The SQLite file
is intact, the outbox is intact and the lease is intact — and not one of them is reachable from the
counter. This is accepted, per store, deliberately, by whoever chooses the mode.

The alternative is a mode that degrades quietly, and every version of it is worse:

- **A device-local write buffer** makes the browser a second writer. Declined below, on its own
  merits, permanently.
- **Read-only browse during an outage** is not selling. A till that displays the menu and cannot
  take money has not degraded gracefully; it has failed with better manners.
- **"It usually works"** fails at the worst possible time. ADR-0001's opening context is that
  *outages cluster at peak hours*. A silent mode is a promise that breaks precisely when it is being
  relied on hardest, in front of a queue.

So the loss is made visible **before** the outage rather than discovered during it. The store's mode
is shown wherever its health is shown: on the fleet console behind `/admin/fleet` and
`/admin/fleet/{store_id}` ([ADR-0068](0068-fleet-liveness.md)), on the store's own detail screen, and
next to the alert ([ADR-0073](0073-alerting.md)) when one fires. "Offline-capable: no" is a fact an
operator can plan around — a second site, a paper fallback, a different mode for that store. A mode
that pretends otherwise is a fact they learn at 19:30 on a Friday.

The mode also changes what a signal *means*, which is why it cannot live only in a provisioning
runbook. A missed heartbeat from an `in-store` placement means "the store may well be selling and we
cannot see it" — that is ADR-0001 working as designed. A missed heartbeat from a hosted placement
means "the store is not selling". Same absence, opposite urgency. Without placement on the record,
the alert engine cannot tell those two apart and must either cry wolf on every rural store or stay
quiet while a hosted store is dark.

### The ADR-0049 / ADR-0108 lease is the sole authority over which placement is active

Switching placement needs no new mutual-exclusion mechanism, because the one that exists is already
the right shape in all four respects that matter.

**It is clock-free.** `lease_standing(held, authoritative)` is a pure function of two generations
with no time input at all — equal ⇒ `Active`, held-behind ⇒ `Superseded`, held-ahead ⇒ `Invalid`
([ADR-0049](0049-single-active-lease.md)). A lease cannot lapse. That mattered for a five-minute
hardware swap; it matters more here, because a placement move is not a five-minute operation. It can
take an evening, or a night, or a week while an operator watches the numbers before committing. Any
time-based handover would fire somewhere in the middle of that, unattended.

**The edge takes its generation once.** The held generation is a row on the store's own SQLite,
inserted with `ON CONFLICT DO NOTHING` (migration
[`0008_lease.sql`](../../crates/adapters/store-sqlite/migrations/0008_lease.sql)), and
[`lease_state.rs`](../../crates/pos-edge/src/lease_state.rs) states the rule in the trait itself:

```rust
/// Two operations and no setter, deliberately. There is no way to move a held generation *forward*
/// on a running box: a machine that must legitimately hold a newer one is a machine being
/// re-provisioned, which starts from a fresh database.
pub trait LeaseAuthority: Send + Sync {
```

That is exactly the failure a placement move invites. The old machine is not destroyed — it is on a
shelf in the back office with the store's whole database on it, and one day somebody plugs it in.
Take-once means it reads the newer generation, computes `Superseded`, and cannot argue its way back
to `Active` on the next config pull.

**It already reaches the store.** The bump writes `store_lease` in Postgres and publishes
`{"generation": N}` as a derived Store-layer `lease` config node in the same action; no admin route
accepts a `lease` node body ([ADR-0108](0108-the-lease-generation-is-authority.md)). The rail that
carries it is the config pull the edge already runs from wherever it is.

**It is already deployable.** A store with no `lease` node is weighed as eligible, so nothing about
this changes a fleet in which no store has ever been issued one. And the held generation already
rides the heartbeat body ([ADR-0068](0068-fleet-liveness.md)), so a console can put "this placement
holds 3, the store's authority is 4" in front of a person.

**Therefore switching placement is bumping the lease, and that is the whole mechanism.** No new
table, no distributed lock, no placement-level "active" flag, no new failure mode.
[ADR-0003](0003-cattle-not-pets.md) already describes this act — the cloud issues a lease naming the
single active server, and the revived old machine finds its lease gone. The only thing this record
adds is that the replacement may be in a different building.

The obvious alternative is wrong, and worth naming so nobody rebuilds it. Giving the store record a
`placement` field with its own `active` flag creates **a second authority over one question**. Two
authorities over single-writer is how a system gets two writers: not on the day they are added, but
on the day they disagree, when a placement row says one thing and a lease generation says another
and nothing in the tree states which wins. Writing the tie-break is where duplicate receipt numbers
come from. The lease already wins. `placement` records *what kind of machine* holds the store; the
lease records *which machine is the store*, and only the second one is authority.

### The handover is three states, and each is an observable fact

**`taking-over`.** The new placement holds a generation the old one does not. Entered by the bump,
and by nothing else. Observable: the authoritative generation is `N+1`, the new placement's heartbeat
reports it holds `N+1`, and the old placement's last heartbeat reports `N` or has stopped arriving.

**`settled`.** The new placement holds the authoritative generation **and** the old placement has
nothing left to send — `outbox_depth == 0` on its heartbeat ([ADR-0068](0068-fleet-liveness.md)), or
the old placement's outbox has been read directly and proven empty. Both halves are required. A new
placement that holds the lease while the old one still has forty unsent sales is not settled; it is
mid-handover with a deadline nobody set.

**`retired`.** A person has looked at a settled handover and decided the old placement's database,
machine and hosting are no longer needed. That decision is the state; there is no automatic
transition into it.

**None of these is defined by elapsed time, and that is deliberate.** A clock here would reintroduce
precisely what ADR-0049 removed. A store cut off for three days is normal, not an error — that is the
entire premise of `in-store` mode. A timer would declare a handover settled while the old machine
still holds the only copy of a night's trading. Both facts the states rest on are already in the
cloud: the authoritative generation is a row it owns, and the held generation and outbox depth arrive
in the heartbeat's optional JSON body, which is already there and already `Option`al so an older edge
that sends neither simply is not yet provably settled.

**Nothing is deleted before `settled`.** Not the old SQLite file, not the state directory, not the
VPS, not the systemd unit. This is a rule about data, not about tidiness. Between the last successful
publish and the switch, the outbox is the **only** copy of those events —
[`event_publish.rs`](../../crates/pos-edge/src/event_publish.rs) is explicit that when a batch does
not land, *"events stay in the outbox and the store keeps trading"*. A store that traded through a
WAN outage carries the sole record of that trading on the machine somebody is about to wipe.

### Both directions use the same choreography

Moving in-store → hosted and moving hosted → in-store are the same five steps: stand the new
placement up, activate it ([ADR-0050](0050-activation-code-exchange.md)), bump the lease, wait for
`settled`, retire the old one.

The direction changes what "stand up" means — a VPS in a region, or a mini-PC carried into a shop —
and it changes what the store gains or loses, which is the offline guarantee. It changes nothing
about the sequence, the states, or what proves each one.

A separate reverse procedure would be a second thing to get right, and it would be the thing you
reach for when the first one has just gone badly — at the moment of least patience and most pressure.
The rollback path has to be the boring one, and it is boring precisely because it is the same path
run the other way. A store that moves to a hosted placement, finds its connectivity worse than
expected, and moves back is running a routine it has already run once, not an emergency it is
inventing.

### A store must never have two writers, and the lease already forbids it

The invariant is one store, one SQLite database, one lease, one process that may write. Every reason
is already recorded and every one is concrete:

- **Receipt numbers.** [ADR-0025](0025-receipt-number-authority.md) is titled for the constraint: the
  per-store counter is gapless *only while one store authority is reachable*. Two writers mint the
  same number, and no care inside a transaction fixes it, because they write different files.
- **Legal invoice numbers.** ADR-0049 calls a duplicate legal invoice number *"the one duplication a
  tax authority will not forgive"*, and built the disjoint forward-only invoice range for it.
- **Stock.** Inventory is consumed at fire ([ADR-0079](0079-inventory-and-suppliers.md)) as a
  decrement against one row. Two writers double-spend the last portion, and auto-86 then hides an
  item that exists or sells one that does not.
- **Shift totals.** A Z report is a sum over one store's events. Two writers produce two partial
  truths, and there is no later reconciliation that recovers which cash drawer held what.

The lease forbids all four without anything new. Exactly one generation is authoritative, exactly one
machine can hold it because the take is once, and every other machine computes `Superseded` or
`Invalid` the first time it reads the node. Placement does not weaken this and does not extend it.
It moves the machine; the lease still names which machine is the store.

## What this deliberately does not do

- **No device-local write buffer. Declined outright, and it stays declined.** The proposal is
  obvious — let the browser queue sales while a hosted edge is unreachable, and drain them when it
  returns — and it is wrong for reasons that do not improve with effort. It is a second writer with
  better manners: receipt numbers, stock and shift totals all assume a single authority, and a buffer
  that allocates none of them cannot produce a receipt, while one that allocates them has collided
  with the edge already. The merge has no correct answer either — two tablets that each issued
  "receipt 412" cannot be reconciled after the fact, only chosen between, and choosing means one real
  customer's real receipt is wrong. And it would need pricing, tax and campaign evaluation in the
  browser: a second definition of the domain, in a language with no `pos-core`, tested by nobody, and
  free to disagree with the first. A store that needs to sell offline chooses `in-store`. That is
  what the axis is for.
- **It does not merge edge logic into `pos_cloud`.** Isolation is the point of the split, not a side
  effect of it: one store's process, one store's SQLite file, one store's blast radius, one store's
  restart. A multi-tenant seller inside `pos_cloud` puts hundreds of stores behind one deploy, one
  connection pool and one bad migration, and it forfeits `in-store` mode entirely, because the thing
  that sells offline has to be the thing in the shop.
  [ADR-0002](0002-one-binary-per-tier.md)'s "one binary per tier" is kept here, not broken: `pos_edge`
  remains the store tier and gains a choice of address.
- **It does not build the host tier.** The console's Start button, the region choice, the supervisor
  that starts and stops edge processes, the per-placement isolation and the backups that follow from
  it are ADR-0113. This record names mode 3 and asserts it is real; it does not stand up one process.
- **It does not change the printer's last hop.** In-store, the edge opens a device path —
  `/dev/usb/lp0`, `/dev/ttyUSB0`, `\\.\COM3` — and writes bytes
  ([ADR-0103](0103-directly-attached-printers.md)). From a hosted placement that path is on the wrong
  machine, and `docs/architecture.md` additionally requires that printers which open a cash drawer be
  USB-attached *or* that all POS devices sit on a separate VLAN, because port 9100 has no
  authentication. Dispatching print from a designated POS terminal, and what happens to the drawer,
  is ADR-0112. Everything *above* the last hop is unchanged
  in every mode: the edge still renders the receipt, still rasterises non-ASCII scripts
  ([ADR-0102](0102-printing-any-script.md)) and still composes the legal invoice block
  ([ADR-0106](0106-the-store-is-a-legal-person.md)), because all of that is Rust on the edge and none
  of it knows where the edge is.
- **It does not decide the operator's legal basis or where data may rest.** Which region a
  platform-hosted placement runs in, whether that crosses a border, and what has to be recorded about
  it are ADR-0114's subject. This record makes the mode visible; it does not make it lawful.
- **It does not give the edge a second address.** A hosted edge is not on the LAN, so ADR-0030's
  raw-IP pairing URL has no IP to carry, `EdgeConfig::advertised_ip` has nothing to mean, and
  [`ui/src/api/client.ts`](../../ui/src/api/client.ts)'s *"one `fetch` to the same origin that served
  the app, so it works on the store LAN with no configuration"* keeps its first clause and loses its
  second. That is ADR-0111, and until it lands a hosted placement has no supported way for a device
  to reach it.
- **It does not open a cloud-to-store channel.** [ADR-0062](0062-the-relay-wake.md) refused one on
  merit, and being hosted is not a new argument for it — a hosted edge dials out exactly as an
  in-store one does, over the same `/sync` routes with the same per-store key. Placement changes the
  network the packets cross, not their direction.
- **It does not rest on either unproven spike.** Tauri v2 against the real `ui/dist` on Android has
  not been spiked, and Android as an ESC/POS print agent over Bluetooth or USB-host has not been
  spiked. Both belong to ADR-0112. If both fail, this record is unaffected: placement is still three
  modes, the lease still moves the store between them, and hosted printing falls back to a designated
  terminal that is not an Android device.

## Consequences

- The cloud's store registry gains a `placement` column with three values and no value meaning
  "unknown". Every existing store is `in-store`, which is not a default — it is what those stores
  actually are.
- `/admin/fleet` and `/admin/fleet/{store_id}` carry the placement, and the fleet console renders it
  beside liveness rather than on a settings page nobody opens during an incident. A store's mode is
  part of its health, because it decides what its health means.
- The alert engine ([ADR-0073](0073-alerting.md)) reads placement. A stale heartbeat from a hosted
  placement is a store that is not trading; from an `in-store` placement it is a store that probably
  is. Same evaluator, two severities, one new input.
- Changing a store's placement is an `/admin` write and inherits everything that implies: `If-Match`
  optimistic concurrency ([ADR-0094](0094-console-optimistic-concurrency.md)),
  `ConsolePermission::ManageStores` ([ADR-0067](0067-multi-admin-console-rbac.md)) — the same
  permission that bumps the lease, because moving a store's placement and replacing its machine are
  the same class of act — and an audit entry naming the acting admin
  ([ADR-0069](0069-audit-trail.md)).
- **Every feature is now tested twice**, and this is the real, recurring cost of the axis. Not the
  domain, which is placement-blind, but everything touching a device path, the LAN, or the assumption
  that the browser and the edge are one hop apart: printing, pairing, discovery, and the `/ws`
  fan-out's under-50 ms budget ([ADR-0018](0018-http-websocket-stack.md)), which was measured across a
  shop and is not a WAN number. Any test that says "the store is offline" now has to say which mode it
  is testing, because in one mode that is a supported state and in the other it is an outage.
- **The outbox drain gap in [`server.rs`](../../crates/pos-edge/src/server.rs) becomes load-bearing.**
  Graceful shutdown today drains in-flight HTTP and only that —
  `axum::serve(...).with_graceful_shutdown(wait_for_shutdown(shutdown_rx))` — and the publish loop is
  one of the background tasks that simply stops when the shared shutdown flips. In-store that costs
  little: the machine boots again in the same shop and the outbox drains. A placement being retired
  does not come back. So **the drain must close before any placement is torn down**, and until it
  does, `settled` is proven by reading `outbox_depth` off the heartbeat rather than by stopping the
  process and hoping.
- **ADR-0002 now has a question it does not answer.** Mode 3 needs something to start, stop and
  supervise edge processes, and that something is a third thing to install on a tier that has one
  binary today. ADR-0113 answers it. This record only records that the question is now open and names
  who closes it.
- `deploy/edge/` and [`installer.rs`](../../crates/pos-edge/src/installer.rs) stay as they are.
  The Linux installer's systemd `StateDirectory`, two slots and atomic symlink rename are exactly
  what a hosted Linux placement wants, so hosted placements get OTA for free. Windows still has a
  service wrapper and no generated installer (issue #182); a hosted placement is a Linux placement,
  which neither fixes #182 nor waits on it.
- [`docs/glossary.md`](../glossary.md) needs correcting in the same change. **Edge** is defined there
  as *"the in-store runtime (`pos_edge`) and the machine it runs on"*, and after this record that
  definition is false for two of three modes. **Placement** joins the table, so that five ADRs and
  the console mean one thing by the word.
- Nothing additive breaks. No published field, event or permission is removed or renamed; a
  `placement` column, one read field on two existing fleet routes and one glossary row are additions,
  and no `PROTOCOL_VERSION` moves — the edge is not told its placement, because it does not need to
  know.
