# ADR-0118 — One credential per box, and the cloud learns what a store admitted

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-08
**Relates to** [ADR-0030](0030-pairing-and-offline-auth.md) (pairing is edge-local and offline — **not reversed here**) · [ADR-0041](0041-device-onboarding.md) §55-57 (**argued against**, not cited) · [ADR-0086](0086-edge-keyvault-and-activation.md) (activation: the box's cloud credential) · [ADR-0091](0091-durable-edge-auth-state.md) §135-137 (**amended** — this is the slice it named) · [ADR-0117](0117-a-headless-store-keeps-a-log.md) §60 (the mint route is the follow-up it scheduled) · [ADR-0004](0004-cloud-owned-configuration.md) §10 · [ADR-0069](0069-audit-trail.md) §38-42 · [ADR-0035](0035-retention-and-pii-masking.md) · [ADR-0076](0076-subject-request-tooling.md) · `docs/pos-spec.md` §9

## No push channel is needed, wanted, or opened

Stated first because a reader who sees *"the cloud knows a store's tills"* will assume otherwise, and
because it is the constraint every option below had to satisfy:

* [ADR-0039](0039-config-delivery.md):14 — *"there is no cloud→store push channel, by
  design."*
* [ADR-0031](0031-cloud-adapter-transports.md) / [ADR-0053](0053-cloud-sync-port.md) — `MessageLink` is
  **outbound only** (`crates/pos-ports/src/cloud_sync.rs`).
* [ADR-0062](0062-the-relay-wake.md):56 — *"There is no live cloud→store connection.
  Refused on merit, not deferred."*
* ADR-0117 rejected a loopback reveal route for the same reason among others.

Everything decided here is either an **outbound event on the durable outbox** the store already runs,
or **pullable state on the existing 30-second config poll**
(`CONFIG_POLL_INTERVAL`, `crates/pos-edge/src/server.rs`). `CloudSync` gains no method. Nothing here
lets the cloud dial a store.

## Context

Two separate mechanisms admit two separate kinds of thing, and they are constantly conflated —
including by this repository's own guide, which is where the confusion comes from:

**Activation** (ADR-0050, ADR-0086) is the **box** getting a cloud identity. The console mints
`XXXX-XXXX-XXXX`; the edge redeems it over HTTPS and stores **one** `SecretName::DeviceCredential`
in the machine's OS keyring. `boot_standing` reads that one secret as the definition of *activated*,
and no cloud loop runs without it.

**Pairing** (ADR-0030) is a **tablet** being admitted to a store. The **edge** mints a six-digit code
from the OS CSPRNG, holds it in memory for five minutes, and hands the redeemer a 128-bit bearer token
that lands in that browser's `localStorage`. No cloud is involved, by decision, because pairing a
tablet during an internet outage is exactly when a store needs it.

The question this record settles is whether the second should become the first: **should the cloud
issue a licence per device — per till, per printer, per kitchen display — with an offline path in which
an already-active machine admits a new one and syncs back later?**

### What is true today, and what is not

**A per-tablet cloud licence does not exist in any form, and the tree cannot hold one.** `SecretName`
is a closed enum with a single `DeviceCredential` variant keyed by its label, so a second `store()`
overwrites the first; a box that already holds one answers **409 CONFLICT** to any further activation
(`crates/pos-edge/src/activation.rs`). Only `pos-edge` ever redeems an activation code — no tablet, no
browser, no print agent does. So today, **whichever named device's code is typed first becomes the
box**, and every other code the console issued for that store is unredeemable. That is a live defect
in the operator experience, and correcting the guide is part of this work.

**The sync-back does not exist in any form either.** The heartbeat carries liveness, outbox depth,
lease generation and print-agent state; the `/sync` report carries the installed version and the
self-test verdict; the event catalogue has exactly one device event and it is activation. A tablet
paired during an outage is invisible to the cloud **forever**. And even where a device fact does reach
the cloud, nothing reads it: `device.activation.completed` already rides the outbox and the projector
matches only the three sales and billing events, so it is an event with no reader — which is precisely
the state a naive sync-back would land in.

**The box's own cloud credential authenticates nothing yet.** `device_credentials` is INSERT-only —
there is no `SELECT` of `secret_hash` anywhere — and `/sync` still runs on the tenant-scoped sync key.
Building a second licence tier on a first tier that has no gate to present itself at is spending the
budget in the wrong order.

**And a cloud licence cannot name a specific tablet even in principle.** A browser till presents no
identity of its own; its device id is minted **by the edge** at redemption from the clock plus entropy,
and `pairing.rs` says outright that *the cloud's approved-device registry is a separate identity this
local id does not claim to be*. Clearing browser data creates a new device (ADR-0111). There is
nothing stable to issue a per-tablet credential **to**. The framework has no device attestation at all
— `print_agent.rs`: *"a paired device is whatever holds a token"* — so a licence would buy naming,
auditability and remote revocation, and never proof of which machine.

### The thing that has to be said plainly

**Admitting a tablet is a physical act, and no licence model changes that.** What is being delivered is
a secret into one specific browser's storage; that browser must reach the edge and receive it. Cloud
issuance changes only *where* the six digits are minted and *who* authorised the slot — never who is
holding the tablet. The proof is already in this tree: the box's credential **is** cloud-issued and
still requires a code typed at the box.

So "an admin sits in one place and supports everything" is not blocked by where codes are minted. It is
blocked by the cloud being **blind**: it can see that a store is wrong and never why, and its picture of
a store's tills is literally zero rows.

## Decision

**Build the visibility half. Do not build the licence half.**

1. **One cloud credential per box, forever.** The fleet converges on the box holding a single
   `DeviceCredential`; a till, printer or kitchen display gets an **entitlement and a name in the
   registry**, never a secret of its own. Answering this in a record is the point of the record — it is
   what stops the per-tablet-credential design walking back in at every review, and what makes the
   guide's *"trades on its own credential"* correctable rather than arguable. Cloud-minted per-device
   certificates stay where ADR-0030 §29-30 put them: a good end state, deferred, and paired with the
   per-store mTLS work rather than with this.

2. **Admission stays edge-local, offline and unconditional.** The admit path keeps exactly the four
   refusals it has — the attempt budget, the lockout, an unknown code, an expired code — every one of
   them derived from state the store owns alone. **No refusal on the admit path may derive from a
   document the store did not author.** This is not conservatism: a cloud-derived refusal is authority
   in the wrong tier, it is what ADR-0030 §27-28 reserved to the store, and the appeal against a wrong
   one would have to travel over the channel ADR-0062 refused.

3. **A signed-in manager on a paired device can mint the next code** — `POST /api/pair/codes`, on the
   **domain** router so it inherits both the paired-device gate and the signed-in gate, with
   `Permission::ManageDevices` checked in the handler against the published roster. It replaces the
   live code rather than adding one, and the response body is the only delivery channel: the handler
   writes no file and logs no code. This is ADR-0117 §60's named follow-up, and it removes *N−1* of the
   service restarts ADR-0117 §70 records as the price of commissioning tills.

4. **The cloud learns admissions by event, not by asking.** Two catalogue additions beside
   `DeviceActivationCompleted`: `device.admission.granted` v1 and `device.admission.revoked` v1
   (**amended at implementation**: this record first wrote them as `device.admitted` and
   `device.revoked`, which the catalogue's own test refuses — every token is `domain.resource.action`,
   one taxonomy shared with permission identifiers, `docs/naming-and-api.md` §5. A published token can
   never be renamed, so it was corrected before it shipped rather than after). They ride the existing
   durable outbox — at-least-once, idempotent by event id — so an admission made during a WAN outage
   reaches the cloud when the link returns, which is exactly the sync-back half of the proposal that
   prompted this record. **A cloud-side reader is part of the same slice**: a projector arm, a
   `local_device_id` column, and a console column that finally shows a store's real tills. An event
   with no reader is not a feature, and there is already one of those in the tree.

5. **The cloud-bound event carries device ids and no actor.** `device.admission.granted` names *what* was
   admitted and *which paired device authorised it* — never which employee. Who admitted a device is
   recorded **locally**, as an additive `actor` column on `paired_devices`, on the store's own SQLite
   and nowhere else; ADR-0091 §147-148 deliberately gives that port no store-postgres adapter, and this
   record keeps it that way. See *Compliance posture* below: this is the decision that keeps a durable,
   central, attributable managerial-activity record from being created as a side effect of an
   observability feature.

6. **Remote revocation is a monotone deny-list of local device ids on the config rail.** Shaped on
   `fleet_update.revoked_key_ids` — the one precedent the edge's config apply permits, and it permits it
   *because it is a deny-list and not an authority*. Applied one-shot per device id and recorded locally
   as applied, never re-applied: because a re-pair mints a **fresh** device id, a manager who
   legitimately re-pairs the till is not fighting the list, which closes what would otherwise be a
   thirty-second re-revoke loop. **Blast radius capped**: a document that would revoke more than one
   bound device in a single apply, or leave the store with zero admitted devices, is refused **whole**.
   A fleet-wide retirement stays the local break-glass it already is. Revocation must be monotone
   rather than a mutable status on a node, because a config rollback performed for an unrelated reason
   must not un-revoke a stolen tablet.

7. **`ManageDevices` is not flipped to `pin: true`.** `pin_required` is read only on the decide path, so
   the literal would be **inert** on an HTTP mint route while making `permissions.txt` advertise a PIN
   that nothing enforces — including on the already-shipped print-agent route. If a PIN at mint is
   wanted it is a hand-rolled `Approval { code, pin }` on the handler, and it must be documented for
   what it is: a defence against an unattended signed-in till, worth **zero** against a dishonest
   manager, because it re-uses the credential they just signed in with. `docs/snapshots/permissions.txt`
   does not change.

8. **Deleted from the design, explicitly:** no `device_licences` node, no slot count, no grace slots,
   no `409` on `POST /api/pair`, no provisional device state, no per-tablet credential. Entitlement is
   enforced where entitlement belongs — in the console, on the roster, after the fact — and the edge
   reports. If commercial seat enforcement is ever genuinely required it is a separate decision with a
   separate record, and it must answer the offline dead end below before it gets one.

## Compliance posture — and what it forced

Decision 5 is not a detail. A `device.admission.granted` event carrying an employee identity would create a
**durable, central, cross-border, attributable record of managerial activity**: which named manager
admitted which device, when, in which store — held in the cloud, replicated by its backups, and
retained under whatever the cloud's retention says rather than the store's.

That is the same class of artefact ADR-0117 §6 deliberately kept off durable storage when it excluded
the sign-in and wrong-PIN stream from the log file, and for the same reasons. Under **Decree 13/2023
(PDPD)** it would need a lawful basis established before processing, a stated retention period, a DPIA
for employee-monitoring-adjacent processing, and — because the fact would cross a border to reach the
cloud region — a transfer basis. Under **GDPR** data minimisation asks the prior question: is the
identity needed for the purpose? The purpose here is *the cloud knows which tills a store admitted, and
can revoke one*. That purpose is served in full by device ids. The identity is not needed for it.

So the identity does not go. Three consequences, all of them wanted:

* The observability feature ships without opening a compliance question at all.
* "Who admitted this device" is still answerable, at the box, from the `actor` column, under the store's
  existing retention and within reach of ADR-0035's masking and ADR-0076's erasure.
* If the fleet later genuinely needs the actor centrally, that is an **additive field on a v2 event**
  with its own record — and that record is where the lawful basis, the retention period, the DPIA and
  the transfer basis get settled, with Technology & Innovation leadership, **before** anything is built.
  It is not settled here, and this record does not pre-authorise it.

The local `actor` column is employee data on a store box. It is an employee id — an identifier, not a
person — written once per admission, on a table whose row counts are in the tens.

**ADR-0069 §38-42 is not extended.** The console audit trail has no non-admin actor and cannot name a
store employee; an admission is therefore an event and a local row, not an audit entry. Whether the
audit trail should learn about store-tier actors is an open question this record flags and does not
answer.

## Alternatives rejected

**A per-tablet cloud-issued credential — the literal reading of the proposal.** It has no subject to be
issued to. The pair request carries one field, `code`; the device id is minted by the edge at
redemption; clearing browser data creates a new device. It would be re-issued on every cache clear, and
the vault has no key space for a second credential in any case. It also needs the cloud **at
redemption**, which is ADR-0030 §27-28's rejected option 2 — the one that breaks pairing during the
outage when pairing matters most.

**A cloud-signed, offline-verifiable licence.** The verifier already ships and its `verify` is
synchronous by design. The **signer** does not exist, by explicit decision — *"There is deliberately no
`sign` method anywhere in this framework"* — and `docs/architecture.md` puts signing keys *"never in the
cloud"*. It needs a second cloud-resident keypair, a second baked-in trust anchor, a release-process
change, a base64 decision the tree already flags as ADR-first, and an ADR narrowing that rule: a
quarter of work, and a security-ownership decision before it is an engineering one. And it would not
even protect the enforcement point, because verify-once-at-apply means a boot re-applies a stored,
unsigned document.

**An unsigned `device_licences` slot cap with a `409` on admission** — the strongest version of the
licence half, steelmanned in full and refuted on four independent grounds, three of them fatal.
*(a)* The `409` puts admission authority in the cloud tier: the first refusal on the admit path derived
from a document the store did not author. *(b)* Its offline claim is **false in this tree** —
`apply_origins` runs only inside `pump_once` and the boot restore path does not touch it, so the
entitlement is empty on every boot during an outage, which is precisely the scenario the design existed
to serve. *(c)* The ledger cannot be maintained: device ids re-mint per pairing, browser clears re-pair
the same hardware, and nothing expires a pairing — so occupancy leaks monotonically against a fixed
count, and a four-till store hits `409` on its **fourth-ever cache clear**, offline, mid-service.
*(d)* Its only local escape is "retire every device", which un-pairs the store and then needs a service
restart to get a code back — strictly worse than the ADR-0117 §70 cost it was built to remove.

**Slot-keyed remote revocation.** Cannot reach the admissions the offline scenario creates: a device
admitted offline carries no slot id, so there is nothing to publish a revocation *for* it against, and
ADR-0091 §135 stays open for exactly those devices. Device-id-keyed revocation, fed by
`device.admission.granted`, reaches every device including those.

**Revocation as mutable slot status on a config node.** Undoable by a config rollback performed for an
unrelated reason. Revocation is monotone or it is decorative.

**A `device.discovered`-shaped event on the ingest stream** — which is what decision 4 is, and which
**ADR-0041 §55-57 rejected**. That rejection has to be argued against rather than cited, and it loses
on both of its grounds. *"There is no such event in the catalogue"* is spent: `DeviceActivationCompleted`
now exists and rides this exact rail. *"LAN-scan noise"* does not describe what this is — a discovery
scan emits unbounded machine chatter, while `device.admission.granted` is **one bounded, deliberate human act by
a signed-in manager**, at the rate a store commissions tills, which is a handful per store per year.
ADR-0041 §58-60's rejection of store self-approval also stops applying once decision 8 deletes the slot
cap: the store is not self-approving against a cloud gate, it is doing what it has always done, and the
cloud is only **learning** about it.

**A cloud→edge push channel, or a reveal route.** Refused three times and on merit. Not reopened, and
not needed.

**Doing nothing.** Leaves the cloud with a zero-row picture of every store's tills, ADR-0091 §135-137
open, a lost tablet as *"send someone to the store"*, and the guide still telling operators that
per-device credentials ship today.

## Consequences

* ADR-0091 §135-137 closes: *"It does not report to the cloud… that needs the state to reach the cloud,
  which is a sync change and its own slice."* This is that slice. §147-148's *"the cloud has no use for
  it"* is reconsidered and, deliberately, only half-reversed — the cloud gets the **admissions**, not
  the port.
* A lost tablet stops being a site visit. That is the single operational win here, and it comes from
  the deny-list, which is why the deny-list cannot ship before the sync-back that gives it ids to name.
* **The cloud can sign a working till out mid-service.** That power does not exist today. One office
  mis-click reaches the store inside one thirty-second poll, with no local override, on a console whose
  device column is a ULID. The one-shot, monotone, id-keyed, blast-radius-capped shape is what makes it
  survivable rather than safe; the residual risk is real and is the reason the cap refuses **whole**
  rather than partially.
* Adding a till stops costing a service restart, for every till after the first on a virgin box. The
  boot-time code remains the floor, because the first device on a fresh box has nothing to authenticate
  with.
* The hot path is untouched: one map read plus one SHA-256 per request. No per-request signature
  verification anywhere, and no new work on the path a sale crosses.
* Two events and one route are added; nothing is renamed or removed. Expected snapshot diffs:
  `docs/snapshots/routes.txt` one line, `docs/snapshots/events.txt` two events.
  `docs/snapshots/permissions.txt` **unchanged** — see decision 7.
* **The console roster will accumulate dead rows.** ADR-0091 deliberately defers pairing expiry, so
  every browser clear, OS reinstall and borrowed phone leaves a row nothing removes. Within months that
  undercuts the readability the roster exists for. Pairing expiry or slot reclaim is the named follow-up,
  and it is not in this record's scope.
* **The remote log tail is still the biggest gap, and it is not this.** An admin can see that a store is
  wrong and never why: only aggregate fields reach the cloud. ADR-0078 §58-61 defers the tail and
  ADR-0117 shipped its local half. Ranked honestly, that is win #1 for one-admin-supports-everything;
  the roster and remote revoke here are #2, and the mint route is #3 — probably the pain actually being
  felt.
* **The `/sync` device-credential gate stays open.** Everything here works without closing it, but no
  story in which *"a device credential means something"* is true until `device_credentials` is read on a
  request rather than only written. Named, not fixed.
* An edge caller for `/sync/…/reconcile` still does not exist, so the console's reconcile screen shows
  history it will never receive. Adjacent, separate, still open.
