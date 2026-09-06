# ADR-0110 — Edge placement is a deployment axis, and the lease is what moves along it

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Bounds [ADR-0001](0001-offline-first-store-autonomy.md)'s offline guarantee to `in-store`
· Rests on [ADR-0049](0049-single-active-lease.md) and
[ADR-0108](0108-the-lease-generation-is-authority.md) for mutual exclusion
· Keeps [ADR-0002](0002-one-binary-per-tier.md) and re-reads
[ADR-0003](0003-cattle-not-pets.md)'s replacement as a move, not only a swap
· Names the attribute under [ADR-0010](0010-naming-standard.md)'s enum rule
· Relates to [ADR-0068](0068-fleet-liveness.md), [ADR-0085](0085-edge-cloud-sync-transport.md),
[ADR-0061](0061-order-relay.md), [ADR-0062](0062-the-relay-wake.md)

First of the five records on the **Edge Anywhere** programme. The other four are
[ADR-0111](0111-a-second-origin-may-address-the-edge.md) (a second origin may address the edge),
[ADR-0112](0112-print-agents.md) (a paired device may own a printer's transport),
[ADR-0113](0113-the-host-agent.md) (the host tier and the console's Start button) and
[ADR-0114](0114-region-is-required-recorded-visible.md) (region as a required, recorded attribute).

## The problem

### The edge does not need the shop; the deployment assumes it anyway

Every connection `pos_edge` opens is outbound, without exception. Activation goes out
([`activation.rs`](../../crates/pos-edge/src/activation.rs) over the `CloudSync` port,
[ADR-0053](0053-cloud-sync-port.md), whose field adapter is the separate
[`cloud-sync-http`](../../crates/adapters/cloud-sync-http/) crate,
[ADR-0054](0054-edge-cloud-http-client.md)). The config pull and the heartbeat go out
([`config_client.rs`](../../crates/pos-edge/src/config_client.rs) and
[`heartbeat_client.rs`](../../crates/pos-edge/src/heartbeat_client.rs), over
[`cloud_http.rs`](../../crates/pos-edge/src/cloud_http.rs), which scopes itself to exactly *"the
config-pull, heartbeat, and order-relay transports"*). The updater goes out
([`ota_client.rs`](../../crates/pos-edge/src/ota_client.rs)). Events go out
([`event_publish.rs`](../../crates/pos-edge/src/event_publish.rs)). Cloud-to-store work goes out too,
because [ADR-0061](0061-order-relay.md) made it a pull the store initiates and
[ADR-0062](0062-the-relay-wake.md) refused to make it anything else. [`AGENTS.md`](../../AGENTS.md)
§1 states it as a rule of the system: *"Stores only make outbound connections. The cloud never dials
into a store."*

Nothing in that list cares which building the process is in. `cloud_url` in
[`config.rs`](../../crates/pos-edge/src/config.rs) is a URL; the edge dials it from wherever it is.

The assumption lives everywhere *else*. [`deploy/edge/`](../../deploy/edge/) holds a systemd unit and
a generated PowerShell installer for a machine somebody carries into a shop, and
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
metre takeaway counter, a site whose power and network belong to the landlord:
[`docs/architecture.md`](../architecture.md) asks for an x86-64 box with an SSD that is *never an SD
card*, and some sites will not get one and keep it healthy.

Some operators will not accept a machine they do not control. They run infrastructure already, they
have a change process, and they would rather run the process themselves than have a mini-PC appear
in their shop. The framework is forked and self-hosted by design
([ADR-0044](0044-fork-and-deploy.md)); an operator who already runs `pos_cloud` asking to also run
`pos_edge` is asking for something the code already supports.

### There is no word for it and no rule for what changes

Without a name there is no diff. Nobody can answer "what does this store lose, and what does the
console show differently?" except by reasoning it out again in each conversation. Four other records
depend on that answer, so it is settled once, here, before any of them is read.

## The decision

**A store's edge placement is an attribute with exactly three values — `in-store`,
`hosted-by-operator`, `hosted-by-platform` — that changes where `pos_edge` runs and nothing else
about what it is.**

### The attribute is `edge_placement`, because `placement` is already taken twice

The bare word is not available. It names two other live things in this tree, and between them the
token appears 445 times in `crates/`:

- **`MenuPlacement`** — the catalog's item-in-a-menu placement, published on
  `/admin/catalog/menus/{menu_id}/placements` and
  `/admin/catalog/menus/{menu_id}/placements/{menu_item_id}`
  ([`openapi_admin.rs`](../../crates/pos-cloud/src/openapi_admin.rs)) and written by
  `create_placement` / `update_placement` in
  [`persistence.rs`](../../crates/pos-cloud/src/persistence.rs).
- **The OTA rollout placement** — `/admin/config/ota/placement`, *"where in the rollout it sits"*,
  whose refusal in [`ota_state.rs`](../../crates/pos-edge/src/ota_state.rs) reads *"no placement means
  the device cannot be weighed"*. [ADR-0108](0108-the-lease-generation-is-authority.md) already uses
  the word in that sense in its own prose — *"merging them would let a placement edit touch the
  lease"* — so the collision is live in the ADR set, not only in the code.

**So the store attribute is `edge_placement`: the column, the JSON field and the configuration
vocabulary all carry the prefix, and the Rust type is `EdgePlacement`.** Per
[ADR-0010](0010-naming-standard.md) every published enum is `UPPER_SNAKE_CASE`, prefixed with the
enum name, and carries a mandatory `*_UNSPECIFIED` zero value, so the wire values are
`EDGE_PLACEMENT_UNSPECIFIED`, `EDGE_PLACEMENT_IN_STORE`, `EDGE_PLACEMENT_HOSTED_BY_OPERATOR`,
`EDGE_PLACEMENT_HOSTED_BY_PLATFORM`.

Neither prior use moves. Renaming `MenuPlacement`'s routes or `/admin/config/ota/placement` would
remove a published name, which `AGENTS.md` §1 forbids outright, and it would be the wrong direction
anyway: the newcomer disambiguates itself. The rule is one line and it binds every record, route,
column and screen in this programme — **`edge_placement`, never a bare `placement`, for this
concept.** Prose may say "a store's edge placement" or "a hosted edge placement"; nothing says "the
placement" and means this.

### The three modes, defined by the network and by who is accountable

**`in-store`.** The edge runs on a machine on the shop LAN. The selling devices reach it over that
LAN. It sells with the cable to the internet unplugged. This is every store today, and it stays the
default.

**`hosted-by-operator`.** The same binary runs on a machine the operator owns — a VPS, a rack in
their office, a box in a regional hub. The platform does not provision it, does not have a shell on
it, and learns about it exactly the way it learns about any store: activation, heartbeat, config
pull, events. Like `hosted-by-platform`, it carries a recorded region: the store's data has left the
shop, and where it went is a fact somebody has to be able to read
([ADR-0114](0114-region-is-required-recorded-visible.md)).

**`hosted-by-platform`.** An admin picks a region in the console and presses Start, and the platform
stands the edge up. That is [ADR-0113](0113-the-host-agent.md)'s job to build; this record's job is
to say the mode exists and is real, so nothing downstream is designed as though the console button
were hypothetical.

The line between modes 2 and 3 is **who owns the machine and who is accountable when it stops** —
not technology. Mode 2 and mode 3 run identical software over identical connections. The line that
changes *behaviour* is the one between mode 1 and modes 2/3: whether the selling devices and the edge
are on the same LAN.

The framework stays neutral on where an operator hosts. It does not prefer mode 3, does not price
mode 2 out, and does not hide either. What it insists on is that the choice is **recorded and
visible**, because it changes what a store can do.

### The domain does not change, and that is the entire reason this axis is affordable

One binary. One SQLite schema and one migration series. One lease. One per-store API key on `/sync`
(migration [`0047_api_key_store_scope.sql`](../../crates/adapters/store-postgres/migrations/0047_api_key_store_scope.sql),
[`docs/production-readiness.md`](../production-readiness.md) S1) over
[ADR-0085](0085-edge-cloud-sync-transport.md)'s transport. One outbound sync. One `pos-core`.

An order, a bill, a tax computation ([ADR-0104](0104-multi-component-and-inclusive-tax.md)), a
receipt number ([ADR-0025](0025-receipt-number-authority.md)), a stock decrement at fire
([ADR-0079](0079-inventory-and-suppliers.md)) and a rendered kitchen ticket
([ADR-0100](0100-receipt-and-ticket-printing.md)) are all computed by the same code against the same
rows, whichever mode the store is in. `edge_placement` is invisible to every one of them.

This works because the hard part is already done. A framework whose store dialled *in* would have to
invert its transport to be hosted; this one has nothing to invert, because `AGENTS.md` §1's outbound
rule made every path a hosted edge placement needs the only path that exists. The axis is cheap for
exactly one reason: it was accidentally paid for years ago.

Keeping it cheap is a constraint on everything after this record. **The moment a domain path branches
on `edge_placement`, the framework has two point-of-sale systems and tests one of them.** The axis may
change *where a byte is written* — the printer's last hop, the origin a browser talks to — and may
never change what the byte says. Those two exceptions are [ADR-0112](0112-print-agents.md) and
[ADR-0111](0111-a-second-origin-may-address-the-edge.md), and both are at the outermost edge of the
system, on purpose.

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
runbook. A missed heartbeat from an `in-store` edge placement means "the store may well be selling and
we cannot see it" — that is ADR-0001 working as designed. A missed heartbeat from a hosted edge
placement means "the store is not selling". Same absence, opposite urgency. Without `edge_placement`
on the record, the alert engine cannot tell those two apart and must either cry wolf on every rural
store or stay quiet while a hosted store is dark.

### The ADR-0049 / ADR-0108 lease is the sole authority over which machine is the store

Switching a store's edge placement needs no new mutual-exclusion mechanism, because the one that
exists is already the right shape in all four respects that matter.

**It is clock-free.** `lease_standing(held, authoritative)` is a pure function of two generations
with no time input at all — equal ⇒ `Active`, held-behind ⇒ `Superseded`, held-ahead ⇒ `Invalid`
([ADR-0049](0049-single-active-lease.md)). A lease cannot lapse. That mattered for a five-minute
hardware swap; it matters more here, because an edge-placement move is not a five-minute operation. It
can take an evening, or a night, or a week while an operator watches the numbers before committing.
Any time-based handover would fire somewhere in the middle of that, unattended.

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

That is exactly the failure an edge-placement move invites. The old machine is not destroyed — it is
on a shelf in the back office with the store's whole database on it, and one day somebody plugs it in.
Take-once means it reads the newer generation, computes `Superseded`, and cannot argue its way back
to `Active` on the next config pull.

**It already reaches the store.** The bump writes `store_lease` in Postgres and publishes
`{"generation": N}` as a derived Store-layer `lease` config node in the same action; no admin route
accepts a `lease` node body ([ADR-0108](0108-the-lease-generation-is-authority.md)). The rail that
carries it is the config pull the edge already runs from wherever it is.

**It is already deployable.** A store with no `lease` node is weighed as eligible, so nothing about
this changes a fleet in which no store has ever been issued one. And the held generation already
rides the heartbeat body ([ADR-0068](0068-fleet-liveness.md)), so a console can put "this machine
holds 3, the store's authority is 4" in front of a person.

**Therefore moving a store's edge placement is bumping the lease, and that is the whole mechanism.**
No new table, no distributed lock, no per-mode "active" flag, no new failure mode.
[ADR-0003](0003-cattle-not-pets.md) already describes this act — the cloud issues a lease naming the
single active server, and the revived old machine finds its lease gone. The only thing this record
adds is that the replacement may be in a different building.

The obvious alternative is wrong, and worth naming so nobody rebuilds it. Giving the store record an
`edge_placement` field with its own `active` flag creates **a second authority over one question**.
Two authorities over single-writer is how a system gets two writers: not on the day they are added,
but on the day they disagree, when one row says one thing and a lease generation says another and
nothing in the tree states which wins. Writing the tie-break is where duplicate receipt numbers come
from. The lease already wins. `edge_placement` records *what kind of machine* holds the store; the
lease records *which machine is the store*, and only the second one is authority.

### The `edge_placement` write is the bump itself, so the two can never disagree

**`edge_placement` is written inside the lease bump's transaction, by the same request, and there is
no other way to write it.** [`admin_bump_lease`](../../crates/pos-cloud/src/http.rs) already describes
itself as one request doing *"two things that must not drift apart"* — advancing the counter in
`store_lease` and publishing the derived `lease` node. It gains a third thing that must not drift
apart from those two: the `edge_placement` of the machine that is about to hold the new generation. A
bump that names no value keeps the store's current one, which is exactly ADR-0003's case — a
replacement box in the same shop. A bump that names a different value is an edge-placement move. One
act, one route (`POST /admin/config/lease/bump`), one permission — `ConsolePermission::ManageStores`
([ADR-0067](0067-multi-admin-console-rbac.md)), which already guards the bump because replacing the
machine that *is* a store is store management.

Neither write happens first, and that is the decision. Order them and you open an interval in which
the store record and the lease disagree, and every reader inside that interval is reading a lie:

- **Column first.** Between the column write and the bump, the console says `hosted-by-platform` and
  "Offline-capable: no" for a store whose in-store box still holds authority and is still selling, and
  the alert evaluator scores that box's stale heartbeat as "not trading". That is the cry-wolf failure
  this axis exists to prevent, manufactured by the axis itself.
- **Column at `settled`.** `settled` can arrive hours or days after the bump, because it waits on an
  outbox draining. Until then the console says "Offline-capable: yes" about a store that has been
  hosted since Tuesday. That is the more dangerous direction: a promise of offline trading that breaks
  in front of a queue.

So they are one transaction, under one `If-Match`
([ADR-0094](0094-console-optimistic-concurrency.md)) and one audit entry naming the acting admin and
the generation issued ([ADR-0069](0069-audit-trail.md)). And `edge_placement` therefore means exactly
one thing, everywhere it is read: **the edge placement of the machine that holds the authoritative
generation.** Not the machine that happens to be running, not the machine the devices happen to
reach — the machine the lease says is the store.

**A reader who sees `edge_placement` and the lease disagree is looking at a heartbeat, not at a
contradiction.** In the cloud they cannot disagree; one transaction writes both. What can differ is
`edge_placement` and the generation the store's last heartbeat reported: the column says
`hosted-by-platform` while the heartbeat reports a generation behind the authoritative one. That
combination has one meaning and it is not ambiguous — a superseded machine is still running and has
not been retired. The column is right about which machine is the store; the heartbeat is right about
which machine last spoke. **The one repair that is wrong is editing the column to match the
heartbeat**, and it is unavailable on purpose: there is no route that writes `edge_placement` alone. If
the column looks wrong, it is because the last bump said so, and the way to change what the last bump
said is another bump.

### The handover is three states, and each is computable from facts the cloud already holds

The cloud already has the authoritative generation `A` (the `store_lease` row, migration
[`0051_store_lease.sql`](../../crates/adapters/store-postgres/migrations/0051_store_lease.sql)) and,
on the store's `store_liveness` row, the generation the last heartbeat said it held and that
heartbeat's `outbox_depth` ([ADR-0068](0068-fleet-liveness.md), migrations `0049` and `0051`). One
fact it does not have: `store_liveness` is **one row per store**, so the moment the incoming machine
pings, the outgoing machine's final `outbox_depth` is overwritten and gone. So the bump records the
generation it supersedes — `store_lease.superseded_generation`, nullable, additive, cleared when a
heartbeat arrives carrying that generation with `outbox_depth == 0`, or by an admin who has read a
powered-off machine's outbox directly and says so in an audited `/admin` write.

**`taking-over`.** A bump has landed and `settled` is not yet true. Entered by the bump and by nothing
else. What a person sees: the authoritative generation is `N+1`, and either the store's last heartbeat
still reports `N`, or `superseded_generation` still names `N`, or both.

**`settled`.** Two facts, both required. The store's latest heartbeat reports the authoritative
generation with `outbox_depth == 0`, **and** `superseded_generation` is null. A new machine that holds
the lease while the old one still has forty unsent sales is not settled; it is mid-handover with a
deadline nobody set.

**`retired`.** A person has looked at a settled handover and decided that the old machine, its
database and its hosting are no longer needed. Nothing transitions into `retired` on its own.

**And a fourth answer: there is no handover.** *Added 2026-09-06, with the derivation.* Read
literally, `taking-over` above catches every store that has ever been issued a lease — including one
on generation `0`, which is its **first**, and which supersedes nobody. That would put a
mid-handover badge on a fleet that has handed nothing over, and the badge would never clear, because
no machine was ever displaced for a heartbeat to report on. A store with no lease row at all is the
same answer for a simpler reason. So the derivation is `Option<HandoverState>`, and both cases
report `None`: the console renders nothing rather than a state, which is the honest shape for "this
has not happened". The three states describe a *handover*, and a store that has never had one is not
in the first of them.

*Amended 2026-09-06.* This paragraph first said the decision *is* the state, observable only as an
audited `/admin` write. That does not hold, and the audit module says why in its own contract:
`AuditRecorder::record` is **best-effort** — *"a store failure is the recorder's to log and swallow,
never the caller's to propagate"*, because a mutation that succeeded must not fail because its audit
write did ([ADR-0069](0069-audit-trail.md)). A trail that is allowed to drop an entry cannot be the
durable record of a decision: one swallowed failure and a retired machine silently reads as merely
settled, which is an invitation to keep paying for it or to trust it again. So `retired` gets storage
of its own — `store_lease.retired_at` and `retired_by`, both nullable, written by that audited
`/admin` write. The audit entry still records *who decided and when*; the columns record *that it was
decided*, which is the part a reader must not lose.

*Added 2026-09-06, with the implementation.* Three rules the states above imply and this paragraph
did not say. **Retiring refuses while a handover is in flight**, naming `superseded_generation`: a
machine that may hold the only copy of a night's sales is not one anybody gets to call unnecessary,
so `retired` really is reachable only from `settled`. **A second retirement is refused, not
applied** — it would replace the first decision's who and when in the row whose entire job is to
hold the first. And **a bump clears both columns in the same statement that records the new
`superseded_generation`**: a bump starts a new handover with a new outgoing machine, so a retirement
from the previous one stops describing the row. These columns are the *current* handover; the
history of every retirement is the trail's, and it has it.

The hand-made half of `settled` is the mirror image and ships with them: an audited `/admin` write
in which a person names the generation whose machine they checked. It takes that number rather than
an `If-Match`, because here the named value is the stronger precondition — any bump necessarily
*changes* `superseded_generation`, so a concurrent bump makes the write refuse on its own. It is an
attestation about one specific machine, so naming the wrong one is refused rather than treated as
idempotent: a silent success would put a false attestation in the trail.

`retired_by` holds the admin's ULID and not their email. The trail already carries the address as it
stood at action time, which is where a human-readable actor belongs; copying it into an operational
row the fleet console reads would spread staff personal data into a table nobody classified as
holding any, for no gain.

**A bump while `superseded_generation` is set is refused, naming the field**
([ADR-0096](0096-unprocessable-status.md)) — you do not move a store off a machine whose events are
still on it. The refusal takes an explicit acknowledgement that names the undrained generation,
because two cases genuinely need it: a rollback from a new machine that never came up, and a machine
that is gone for good. Abandoning unsent events is then a recorded decision with a name against it,
which is the only acceptable way to do it.

**None of these is defined by elapsed time, and that is deliberate.** A clock here would reintroduce
precisely what ADR-0049 removed. A store cut off for three days is normal, not an error — that is the
entire premise of `in-store` mode. A timer would declare a handover settled while the old machine
still holds the only copy of a night's trading. Every fact these states rest on is one the cloud
already owns or already receives: the authoritative generation is its own row, and the held generation
and outbox depth arrive in the heartbeat's optional JSON body, which is already `Option`al — so an
older edge that sends neither simply is not yet provably settled.

**Nothing is deleted before `settled`.** Not the old SQLite file, not the state directory, not the
VPS, not the systemd unit. This is a rule about data, not about tidiness. Between the last successful
publish and the switch, the outbox is the **only** copy of those events —
[`event_publish.rs`](../../crates/pos-edge/src/event_publish.rs) is explicit that when a batch does
not land, *"events stay in the outbox and the store keeps trading"*. A store that traded through a
WAN outage carries the sole record of that trading on the machine somebody is about to wipe.

### Both directions use the same choreography

Moving in-store → hosted and moving hosted → in-store are the same five steps: stand the new machine
up, activate it ([ADR-0050](0050-activation-code-exchange.md)), bump the lease naming the new
`edge_placement`, wait for `settled`, retire the old one.

The direction changes what "stand up" means — a VPS in a region, or a mini-PC carried into a shop —
and it changes what the store gains or loses, which is the offline guarantee. It changes nothing
about the sequence, the states, or what proves each one.

A separate reverse procedure would be a second thing to get right, and it would be the thing you
reach for when the first one has just gone badly — at the moment of least patience and most pressure.
The rollback path has to be the boring one, and it is boring precisely because it is the same path
run the other way. A store that moves to a hosted edge placement, finds its connectivity worse than
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
`Invalid` the first time it reads the node. `edge_placement` does not weaken this and does not extend
it. It moves the machine; the lease still names which machine is the store.

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
  that sells offline has to be the thing in the shop. `pos_edge` remains the store tier and gains a
  choice of address.
- **It does not build the host tier.** The console's Start button, the region choice, the supervisor
  that starts and stops edge processes, the per-store isolation and the backups that follow from it
  are [ADR-0113](0113-the-host-agent.md). This record names mode 3 and asserts it is real; it does not
  stand up one process.
- **It does not change the printer's last hop.** In-store, the edge opens a device path —
  `/dev/usb/lp0`, `/dev/ttyUSB0`, `\\.\COM3` — and writes bytes
  ([ADR-0103](0103-directly-attached-printers.md)). From a hosted edge placement that path is on the
  wrong machine, and [`docs/architecture.md`](../architecture.md) additionally requires that printers
  which open a cash drawer be USB-attached *or* that all POS devices sit on a separate VLAN, because
  port 9100 has no authentication. Dispatching print from a designated POS terminal, and what happens
  to the drawer, is [ADR-0112](0112-print-agents.md). Everything *above* the last hop is unchanged in
  every mode: the edge still renders the receipt, still rasterises non-ASCII scripts
  ([ADR-0102](0102-printing-any-script.md)) and still composes the legal invoice block
  ([ADR-0106](0106-the-store-is-a-legal-person.md)), because all of that is Rust on the edge and none
  of it knows where the edge is.
- **It does not decide the operator's legal basis or where data may rest.** Which region a hosted
  edge placement runs in — either mode, not only `hosted-by-platform` — whether that crosses a border,
  and what has to be recorded about it are
  [ADR-0114](0114-region-is-required-recorded-visible.md)'s subject. This record makes the mode
  visible; it does not make it lawful.
- **It does not give the edge a second address.** A hosted edge is not on the LAN, so
  [ADR-0030](0030-pairing-and-offline-auth.md)'s raw-IP pairing URL has no IP to carry,
  `EdgeConfig::advertised_ip` has nothing to mean, and
  [`ui/src/api/client.ts`](../../ui/src/api/client.ts)'s *"one `fetch` to the same origin that served
  the app, so it works on the store LAN with no configuration"* keeps its first clause and loses its
  second. That is [ADR-0111](0111-a-second-origin-may-address-the-edge.md), and until it lands a
  hosted edge placement has no supported way for a device to reach it.
- **It does not open a cloud-to-store channel.** [ADR-0062](0062-the-relay-wake.md) refused one on
  merit, and being hosted is not a new argument for it — a hosted edge dials out exactly as an
  in-store one does, over the same `/sync` routes with the same store-scoped key. The axis changes the
  network the packets cross, not their direction.
- **It does not rest on two spikes nobody has run.** Tauri v2 against the real `ui/dist` on Android
  has not been spiked, and neither has Android as an ESC/POS print agent over Bluetooth or USB-host.
  Neither appears in the roadmap or in [`docs/gate-register.md`](../gate-register.md) today;
  [ADR-0112](0112-print-agents.md) is where both are recorded and where they will be tracked. If both
  fail, this record is unaffected: there are still three modes, the lease still moves the store
  between them, and hosted printing falls back to a designated terminal that is not an Android device.

## Consequences

- **`store_lease`** gains an `edge_placement` column, `NOT NULL`, carrying
  `EDGE_PLACEMENT_IN_STORE`, `EDGE_PLACEMENT_HOSTED_BY_OPERATOR` or
  `EDGE_PLACEMENT_HOSTED_BY_PLATFORM`. Every existing store is `EDGE_PLACEMENT_IN_STORE`, which is not
  a default — it is what those stores actually are.

  *Amended 2026-09-06, after the implementation (#200).* This bullet first said the column went on the
  **store registry**. It went on `store_lease` instead, and the reason is the rule two bullets down:
  the only writer is the bump. On `stores` that is a convention somebody has to keep, because the
  table already carries a rename-and-archive `UPDATE` path, so the column would sit one careless
  statement away from being editable — and an edited column is the repair this ADR names as always
  wrong, because it makes the record agree with a superseded box instead of with the machine holding
  authority. On `store_lease` the property is structural: that table's only write *is* the bump, so
  the placement cannot drift from the generation beside it. It also reads more truthfully — the value
  does not mean "where this shop's computer is", it means *the placement of the machine holding the
  authoritative generation*, which is a fact about the lease. `EDGE_PLACEMENT_UNSPECIFIED` exists because
  [ADR-0010](0010-naming-standard.md) requires every published enum to have a zero value, and it is
  never stored and never accepted on a write: it is what an older reader sees in place of a value a
  newer server added, which is the rule that keeps adding a fourth mode non-breaking.
- `store_lease` gains a nullable `superseded_generation`, and the heartbeat's liveness write clears
  it when the generation it names reports an empty outbox — both facts from the *same* message, never
  from the stored liveness row, which COALESCEs depth and generation independently and can therefore
  hold a pair that came from two different beats. That column is the whole of the handover
  machinery: `taking-over` and `settled` are read off it and the liveness row, with no timer and no
  new table.

  *Amended 2026-09-06, after the implementation.* The clear above describes a heartbeat that, at the
  time this was written, **no stopping store ever sent**. `HeartbeatClient::run` left its loop the
  instant the shutdown watch flipped, and production-readiness **D8**'s last drain runs afterwards in
  `serve_until` — so the final thing a cleanly-stopping machine said was the tick reporting the
  backlog it was about to clear, and the zero it went on to achieve reached nobody. The automatic
  clear could not fire, and every handover, however well it went, still needed a person. The edge now
  holds that last beat back until the drain is done and then sends it
  (`pos_edge::server::FarewellBeat`). It reports the outbox rather than the drain's opinion of the
  outbox, so a drain that ran out of budget reports a truthful non-zero and this clause refuses
  itself, with no error path to plumb.
- `edge_placement` is read on `/admin/fleet` and `/admin/fleet/{store_id}`
  ([ADR-0068](0068-fleet-liveness.md)) and on `/admin/stores` and `/admin/stores/{store_id}`, and the
  fleet console renders it beside liveness rather than on a settings page nobody opens during an
  incident. A store's mode is part of its health, because it decides what its health means.
- The only route that **writes** it is `POST /admin/config/lease/bump`, inside the bump's transaction,
  behind `ConsolePermission::ManageStores` ([ADR-0067](0067-multi-admin-console-rbac.md)) with
  `If-Match` ([ADR-0094](0094-console-optimistic-concurrency.md)) and an audit entry naming the acting
  admin ([ADR-0069](0069-audit-trail.md)). Moving a store's edge placement and replacing its machine
  are not merely the same class of act; they are the same act.
- The alert engine ([ADR-0073](0073-alerting.md)) reads `edge_placement`. A stale heartbeat from a
  hosted edge placement is a store that is not trading; from an `in-store` one it is a store that
  probably is. Same evaluator, two severities, one new input. During `taking-over` it scores the
  machine holding the authoritative generation, and a superseded machine going quiet is not an alert:
  that is what the bump asked for.
- **Every feature is now tested twice**, and this is the real, recurring cost of the axis. Not the
  domain, which is blind to `edge_placement`, but everything touching a device path, the LAN, or the assumption
  that the browser and the edge are one hop apart: printing, pairing and discovery. The `/ws` fan-out
  is one thing this does *not* threaten and it is worth being precise about why: it is an in-process
  `tokio::sync::broadcast`, so [ADR-0018](0018-http-websocket-stack.md)'s under-50 ms budget is *"met
  by construction"* in every mode. But ADR-0018 treats that figure as the **interactive** budget for
  the LAN, and in a hosted edge placement the WAN hop between the edge and the browser sits outside it
  and is covered by nothing. Any test that says "the store is offline" now has to say which mode it is
  testing, because in one mode that is a supported state and in the other it is an outage.
- **The outbox drain gap in [`server.rs`](../../crates/pos-edge/src/server.rs) becomes load-bearing.**
  Graceful shutdown today drains in-flight HTTP and only that —
  `axum::serve(...).with_graceful_shutdown(wait_for_shutdown(shutdown_rx))` — and the publish loop is
  one of the background tasks that simply stops when the shared shutdown flips. In-store that costs
  little: the machine boots again in the same shop and the outbox drains. A machine being retired does
  not come back. So **the drain must close before any machine is torn down**
  ([ADR-0113](0113-the-host-agent.md) closes it), and until it does, `settled` is proven by reading
  `outbox_depth` off the heartbeat rather than by stopping the process and hoping.
- **ADR-0002 is amended twice by this programme, not kept intact.**
  [ADR-0112](0112-print-agents.md) adds the print agent as a third artifact and confronts
  [ADR-0002](0002-one-binary-per-tier.md) directly; [ADR-0113](0113-the-host-agent.md) then adds the
  host agent as a fourth, amending it a second time. Mode 3 needs something to start, stop and
  supervise edge processes, and that something is not a binary the store tier has today. This record
  records that both questions are now open and names who closes them.
- `deploy/edge/` and [`installer.rs`](../../crates/pos-edge/src/installer.rs) carry
  `hosted-by-operator` unchanged: an operator's Linux VPS running
  [`pos-edge.service`](../../deploy/edge/pos-edge.service) gets the systemd `StateDirectory`, the two
  slots and the atomic symlink rename exactly as an in-shop box does, so mode 2 gets OTA for free.
  **Mode 3 does not.** An edge in a container has no `<state>/bin/current`, so
  `SystemdInstaller::is_ready` finds no layout and starts no updater — correct behaviour, and the
  reason [ADR-0113](0113-the-host-agent.md) rebuilds the two-slot swap per host and adds
  `deploy/edge/Dockerfile`. Windows is unaffected either way and is not a gap: it has both a service
  wrapper ([`service.rs`](../../crates/pos-edge/src/service.rs)) and a generated installer
  ([`install-pos-edge.ps1`](../../deploy/edge/install-pos-edge.ps1), emitted by
  [`installers.mjs`](../../dashboard/src/installers.mjs), diff-checked by the `dashboard` CI job and
  syntax-checked by [`installer-syntax.mjs`](../../dashboard/scripts/installer-syntax.mjs)). A hosted
  edge placement is a Linux one regardless.
- [`docs/glossary.md`](../glossary.md) needs correcting in the same change. **Edge** is defined there
  as *"the in-store runtime (`pos_edge`) and the machine it runs on"*, and after this record that
  definition is false for two of three modes. **Edge placement** joins the table under that name, with
  a line saying which two other things in this tree the bare word already means, so a reader who meets
  `/admin/config/ota/placement` or `MenuPlacement` is not misled and five ADRs and the console mean one
  thing by `edge_placement`.
- Nothing additive breaks. No published field, event or permission is removed or renamed — including
  the two older uses of `placement`, which keep their names. What is added is an `edge_placement`
  column and one nullable lease column, a read field on `/admin/fleet`, `/admin/fleet/{store_id}`,
  `/admin/stores` and `/admin/stores/{store_id}`, one optional field on the existing bump request, and
  one glossary row. No `PROTOCOL_VERSION` moves — the edge is not told its `edge_placement`, because it
  does not need to know.
