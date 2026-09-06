# ADR-0114 — Region is a required, recorded, visible attribute of every hosted edge placement

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Completes [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s hosted modes with the one fact
they create
· Written by the lease bump [ADR-0108](0108-the-lease-generation-is-authority.md) owns, recorded
through [ADR-0069](0069-audit-trail.md), behind
[ADR-0067](0067-multi-admin-console-rbac.md)'s `ManageStores` and
[ADR-0094](0094-console-optimistic-concurrency.md)'s `If-Match`
· Supplies the region [ADR-0113](0113-the-host-agent.md) filters every job on
· Leaves fulfilment where [ADR-0076](0076-subject-request-tooling.md) left it — with the operator
· Keeps [ADR-0005](0005-country-neutral-core.md)'s neutrality about countries
· Relates to [ADR-0068](0068-fleet-liveness.md), [ADR-0099](0099-store-hub.md),
[ADR-0070](0070-people-and-access.md), [ADR-0106](0106-the-store-is-a-legal-person.md),
[ADR-0107](0107-the-buyer-is-a-subject.md), [ADR-0035](0035-retention-and-pii-masking.md),
[ADR-0044](0044-fork-and-deploy.md), [ADR-0111](0111-a-second-origin-may-address-the-edge.md),
[ADR-0112](0112-print-agents.md)

Fifth and last of the five records on the **Edge Anywhere** programme, after
[ADR-0110](0110-edge-placement-is-a-deployment-axis.md),
[ADR-0111](0111-a-second-origin-may-address-the-edge.md), [ADR-0112](0112-print-agents.md) and
[ADR-0113](0113-the-host-agent.md). All five are on disk and all five are linked here as documents.

## The problem

### A hosted edge placement moves a store's personal data onto a machine somebody chose

[ADR-0110](0110-edge-placement-is-a-deployment-axis.md) gives a store one new degree of freedom:
where `pos_edge` runs. What travels with the process is a whole store's operational data, and three
parts of it are personal.

**Employee identifiers and PIN hashes.** The `permissions` node
([ADR-0070](0070-people-and-access.md)) reaches the edge over the config pull and becomes the roster
the box authenticates against. [`config_client.rs`](../../crates/pos-edge/src/config_client.rs)
deserialises each staff member as an id, the code a person types, their granted permissions and
`pin_phc` — the Argon2id PHC string
[`auth.rs`](../../crates/pos-edge/src/auth.rs) verifies against. Those hashes sit on that machine's
disk and in its memory, and they are credentials belonging to named people.

**A B2B buyer's name, tax code and address.** [ADR-0107](0107-the-buyer-is-a-subject.md) put the
buyer's record on the store, not in the cloud, because *"the receipt is composed and printed at the
till, at settle time"* and a compliant invoice must not depend on a link. That record is the
`subjects` table in
[`0007_subjects.sql`](../../crates/adapters/store-sqlite/migrations/0007_subjects.sql), whose
`fields` column is documented as *"the JSON `{name, tax_code, address, …}` document"*. ADR-0107 also
settled that the tax code is personal data often enough that it must always be treated so: for a sole
trader, Japan's 登録番号 is issued to the individual and India's GSTIN embeds a PAN.

**Everything the sale itself is.** The outbox, the projection, the shift totals. Not personal by
construction — `pos_proto::pii` makes a name in an event payload a compile error — but a complete
record of one shop's trading.

In `in-store` mode none of this is a question anyone has to ask. The machine is in the shop, so the
data is where the trade is, by construction. Hosted, that stops being true and nothing in the tree
notices.

### Nothing in the framework records where a machine is, and the one thing called "region" is not one

`StoreRecord` in [`registry.rs`](../../crates/pos-cloud/src/registry.rs) is five fields — `store_id`,
`tenant_id`, `brand_id`, `name`, `status`. No address, no country, no location. ADR-0110 adds
`edge_placement`, which says *what kind* of machine holds the store and deliberately not where it is.

The word `region` does appear in the tree, and it means something else entirely.
[`config.rs`](../../crates/pos-cloud/src/config.rs) documents `ArtifactsConfig::region` as *"The S3
region name. Garage's own default is `garage`; it is a signing input, not a location"*, and the
default function says it again: *"It participates in the `SigV4` signature and has to match what the
server was configured with; it does not name a geography."* So the only `region` this framework has
today is a string inside a signature.

The store's **country** does exist, in exactly one place and only optionally: `country_code` on the
[`LocalePack`](../../crates/pos-proto/src/locale.rs) that
[`http.rs`](../../crates/pos-cloud/src/http.rs) publishes as the store's `locale` node on the Store
layer ([ADR-0027](0027-country-modules.md) draws the line between what a country module ships and
what configuration overrides). It is **not** on `store_profile`, which carries the legal and trading
names and the address and no country at all ([ADR-0106](0106-the-store-is-a-legal-person.md)). A
store whose locale was never published has no recorded country at all.

### A framework that defaults, infers or hides is not neutral

The instinct is to be helpful. Every helpful version is the framework making a legal choice on the
operator's behalf and not telling them:

- **Default to the cloud's own region.** This says the platform's home is everybody's home. It would
  be right in the fleet's first country and wrong in the other four, and wrong silently.
- **Infer from the store's country.** This assumes the answer to the question the field exists to
  ask. The entire reason to record a region is that it may differ from the store's country; a field
  that is derived from the country can never disagree with it, and therefore records nothing.
- **Record it and show it nowhere.** A column in a table nobody reads is not a disclosure. The
  question arrives months later — during an audit, or when a subject request under
  [ADR-0076](0076-subject-request-tooling.md) asks where a person's data has been — and "we would
  have to check the database" is the same answer as "we do not know".

### The duty is already the operator's, and the gates already say so — they just have no fact to point at

This framework has taken one position on privacy law consistently and it is the right one.
[`gate-register.md`](../gate-register.md) **L3** requires a human to *"clear any cross-border
transfer — a DTA or explicit consent — before data leaves the country of collection"*, and names what
it blocks: *"Any hosting region outside it."* [ADR-0070](0070-people-and-access.md) says *"a
deployment that replicates the cloud across regions must confirm a DTA/consent first (flagged, not
code's call)"*. [ADR-0076](0076-subject-request-tooling.md) is blunter: the tooling *"records and
enables a human decision; it does not make one"*, and cross-border transfer of an exported payload is
*"the operator's obligation to clear"*.

L3 is the gate this record serves, and it is written about a fact nothing in this repository holds.
Its neighbour is not: **X3** sits in the register's *"External registration — a third party must
act"* section and reads *"Where the **cloud** may run for those countries"* — a decision about a
Cell, which [`glossary.md`](../glossary.md) defines as *"one complete, independent cloud deployment
serving one country"*. Nothing here moves `pos_cloud`, so nothing here touches X3.

L3 was written as a fleet-wide decision because a per-store answer did not exist. ADR-0110 makes the
answer differ per store, which is what turns a flagged gate into a missing column.

## The decision

**Every hosted edge placement carries a region an admin chose, the trail records who chose it, the
console shows it beside the store's country, and the framework attaches no rule of its own to the
value.**

Mechanism, never policy. What the region *means* — whether the edge placement is lawful, whether a
transfer is covered, whether anyone was told — is the operator's, and this record says so in the
places an operator will look.

### The attribute is `edge_placement`, because the bare word is already taken twice

ADR-0110 settles the vocabulary and this record inherits it without variation. `placement` is already
taken twice in this tree: **`MenuPlacement`**, the catalog's item-in-a-menu placement
(`/admin/catalog/menus/{menu_id}/placements`, `create_placement` / `update_placement`), and the **OTA
rollout placement**, `/admin/config/ota/placement` — *"where in the rollout it sits"* — whose refusal
reads *"no placement means the device cannot be weighed"*. Neither prior use moves, because removing
a published name is forbidden outright.

**So the store attribute is `edge_placement`: the column, the JSON field and the configuration
vocabulary all carry the prefix, and the Rust type is `EdgePlacement`.** Per
[ADR-0010](0010-naming-standard.md) every published enum carries a mandatory `*_UNSPECIFIED` zero
value and `UPPER_SNAKE_CASE` wire tokens prefixed with the enum name, so the values are
`EDGE_PLACEMENT_UNSPECIFIED`, `EDGE_PLACEMENT_IN_STORE`, `EDGE_PLACEMENT_HOSTED_BY_OPERATOR` and
`EDGE_PLACEMENT_HOSTED_BY_PLATFORM`. Prose in this record says "a store's edge placement" or "a
hosted edge placement"; it never says "the placement" and means this.

### Region is required on a hosted edge placement, and `in-store` does not have one

The region is two fields on the store's edge-placement record:

- **`region_country`** — an ISO 3166-1 alpha-2 code, validated with the same `CountryCode::parse` in
  [`locale.rs`](../../crates/pos-proto/src/locale.rs) that the store-profile publish already calls. It
  is a two-ASCII-letter shape check and nothing more, so a fork placing a store in a country it has no
  compiled module for still records the country rather than being refused.
- **`region_label`** — an opaque string the admin supplies: `ap-southeast-1`, `Ho Chi Minh City`, `the
  rack in Sakai`. It is stored verbatim and **never parsed**. It exists so a person recognises the
  place; the framework reads only the country code. It is non-empty after trimming and at most **64
  characters**, counted as Unicode scalar values rather than bytes, so `堺のラック` gets the same room
  as `ap-southeast-1`.

Both are `Option`, both are absent for `in-store`, and both are **required together** when the edge
placement is `EDGE_PLACEMENT_HOSTED_BY_OPERATOR` or `EDGE_PLACEMENT_HOSTED_BY_PLATFORM`. The
requirement lives in the write, not the column type, because `in-store` legitimately has no region:
the machine is in the shop, and inventing "the shop's region" would be a second, weaker copy of the
address already on `store_profile`.

**Four refusals, each `InvalidArgument` naming its own field** on the [AIP-193](../naming-and-api.md)
envelope every other `/admin` refusal uses, through `api_error_with_details` in
[`http.rs`](../../crates/pos-cloud/src/http.rs): a hosted edge placement with either region field
absent; a `region_country` that is not two ASCII letters; a `region_label` that is empty after
trimming; a `region_label` longer than 64 characters. A region on an `in-store` edge placement is the
fifth — it is refused rather than ignored, because silently dropping a value an admin typed is how a
console teaches people that the field does not matter.

**The set of regions is not closed, and this is the one place the framework differs from its own
habit.** [ADR-0090](0090-tls-postures.md) gave `TLS_MODE` exactly four values and infers nothing from
`DOMAIN`, and that is right: a TLS posture enumerates behaviours the code implements, so a fifth value
would be a behaviour that does not exist. A region names a place in the world. A framework that ships
a closed list of places has decided which places exist, which is the opposite of neutrality and would
be stale the week after a fork opens in a country the list forgot. `hosted-by-platform` still offers a
list — under [ADR-0113](0113-the-host-agent.md) the console offers the regions enrolled hosts actually
declared — but that list belongs to a deployment, not to this repository.

**A region is required per store, not per fleet, and that is the point.** A fleet of 500+ stores
across ten brands and five countries does not have one answer; it has five hundred, and some of them
will differ from their neighbours for reasons the operator has and the framework does not.

### It is written by the lease bump, because that is the only act that writes an edge placement

ADR-0110 is explicit that the only route which writes `edge_placement` is
`POST /admin/config/lease/bump`, inside the bump's transaction, so **the region is written there
too**, as two more optional fields on the bump request beside the `edge_placement` one ADR-0110 adds.
There is no second route, and there must not be: a region that could be edited without moving the
store would let the record say a store rests in Singapore while the machine holding the lease sits in
Tokyo. Moving where a store's data rests and replacing the machine it rests on are the same act,
which is why they are one request.

That request already carries everything this needs: `ConsolePermission::ManageStores`
([`console_rbac.rs`](../../crates/pos-cloud/src/auth/console_rbac.rs), the permission
[ADR-0108](0108-the-lease-generation-is-authority.md) put the bump behind), `If-Match` optimistic
concurrency ([ADR-0094](0094-console-optimistic-concurrency.md)), and an audit entry.

The entry is the bump's own `config.lease.bump`, whose `before` and `after` gain the `edge_placement`
and both region fields beside the generation it already names. [ADR-0069](0069-audit-trail.md)
requires that *"a route whose entity carries personal data must redact it before writing the audit
`before`/`after`"*. A country code is business metadata and goes in whole. The actor is the
`AdminContext` snapshot ADR-0069 copies at the moment of the action (`actor_admin_id`, `actor_email`,
`actor_role`), so renaming or removing that admin later never rewrites who chose the region.

**A "current region" column with no history would be wrong**, and ADR-0069 already argued why in
general: a mutable last-changed-by *"answers 'who last touched this' but destroys the history … that
an audit trail exists to keep."* The argument is stronger here than it was there. The interesting
question about a transfer is never only "where is it now". It is "since when, and who decided", and
that question is asked by someone who was not in the room. An append-only entry answers it; a column
cannot.

**Two of these fields are free text, and this record says so rather than assuming otherwise.**
`region_label` is whatever an admin typed, and so is the acknowledgement's reason below. A framework
that never parses a string cannot promise what is inside it, and a `hosted-by-operator` label naming
somebody's premises would be a person's address in an audit row. Redacting them is not the answer: an
entry whose payload is redacted does not record what was chosen, which is the only thing this entry
exists to record. So both go in whole, and the exposure is bounded at the two places that matter.
Neither field is ever written to a log line or into an event payload — `AGENTS.md` §2 forbids both
absolutely, and `pos_proto::pii` makes the second a compile error. And the console asks for the right
thing at the input: the label's helper text asks for a place and not a person, and the reason's asks
for the operator's ground and not a customer's details. The residual is an admin who types a name
into a field asking for a place; it is named here rather than designed away, exactly as
`RCLONE_REMOTE`'s gap is named below.

### It is visible beside the store's country, and the comparison happens where the country can be read

Three surfaces, and none of them needs a new route:

- **Fleet.** `/admin/fleet` and `/admin/fleet/{store_id}` ([ADR-0068](0068-fleet-liveness.md)) carry
  both region fields beside the `edge_placement` ADR-0110 puts there, and
  [`Fleet.tsx`](../../dashboard/src/screens/Fleet.tsx) renders them as a column.
- **The store hub.** [ADR-0099](0099-store-hub.md)'s rule is that *"each card reads an endpoint that
  already exists"*, and [`StoreHub.tsx`](../../dashboard/src/screens/StoreHub.tsx) already calls
  `api.fleetStore(tenant, store)` for its Online and Configuration cards. Region rides that read. That
  is what keeps this one screen of work instead of a track.
- **The stores table.** [`Stores.tsx`](../../dashboard/src/screens/Stores.tsx) is where a store is
  created, renamed and archived, so it is where somebody scanning a tenant will see it.

ADR-0110 puts `edge_placement` *"beside liveness rather than on a settings page nobody opens during an
incident"*. Region follows the same rule for the same reason, one step further out: the incident here
is not an outage, it is a question asked in a meeting, and the answer has to be on the screen the
person already has open.

**The country it is compared against is a config-tree node, so the comparison belongs to the
single-store reads.** `locale.country_code` is written by `publish_config_nodes` onto the store's
Store layer in [`http.rs`](../../crates/pos-cloud/src/http.rs), and `ConfigTreeStore::load` is
keyed `(tenant, store)`. `FleetState` is `{ fleet, admin, clock }` and `RegistryState` is
`{ registry, admin, clock, audit }`; neither holds a config-tree handle today. So
`/admin/fleet/{store_id}` and `/admin/stores/{store_id}` gain one — a `ConfigTreeStore` field on their
state — and do exactly one `load` for the one store they already name. The hub inherits the comparison
through `api.fleetStore`, so ADR-0099's rule holds: no new endpoint, one more field on a read that
already runs.

**The two list routes carry the region and do not compare it, and that is a decision rather than an
omission.** A comparison on `/admin/fleet` or `/admin/stores` would be one config-tree load per row —
work that grows with the fleet, which `AGENTS.md` §2's bounded-work discipline and
[ADR-0098](0098-paged-admin-reads.md)'s read discipline both refuse. The tempting alternative is
worse than the read it saves: copying `country_code` onto the store's registry row so the list gets it
free would create a second copy of a versioned node, and config rollback
([ADR-0094](0094-console-optimistic-concurrency.md), G2) can move the node backwards while the copy
keeps asserting the old value — a silent disagreement about a store's country, which is precisely the
failure this record exists to prevent. So the list says where a store's edge is, and the store's own
screen says whether that agrees with the store's country. The region cell on a list row links into the
store, which is the screen that can answer the question.

**Where the store's country comes from, and what happens when there is none.** The comparison value is
`locale.country_code` ([ADR-0027](0027-country-modules.md)), which is optional. When
a store has never published a locale, the console shows **"country not recorded"** beside the region
and makes no comparison. Absence must never render as agreement. Showing the gap is honest, and it
puts the missing profile in front of the one person who can publish it.

### A mismatch warns. It never blocks, and the dismissal is audited

When `locale.country_code` and `region_country` are both present and differ, every surface that
performs the comparison shows a warning against that store. The bump still goes through, the edge
placement still moves, the lease still bumps.

**Blocking would be wrong, and not marginally.** A block is the framework asserting that this
particular cross-border edge placement is unlawful. Whether it is depends on the basis for the
transfer, whether an agreement or an adequacy finding covers it, whether consent was taken and what was
said when it was — facts held by the operator, under law that changes without a release, in
jurisdictions the framework was never told about. This repository is forked and self-hosted by design
([ADR-0044](0044-fork-and-deploy.md)); a block encodes one country's rule into a tool run by operators
in five, and it is wrong in **both** directions. It refuses edge placements that are perfectly lawful —
a move between two countries with a standing agreement, a group that has cleared exactly this transfer
— and it catches none of the unlawful ones, because a same-country edge placement with no lawful basis
at all passes every check a framework could write. A block is a check that fails the honest case and
misses the dishonest one.

And a block that is wrong gets routed around. The admin picks whichever region passes, and the record
then says something false — which is worse than no record, because the next person believes it. **A
dismissed warning is recorded. A dodged block is not.**

**Silence would be worse than either.** ADR-0110's argument about the offline promise applies word for
word: the loss is made visible *before* the event rather than discovered during it. Here the event is
an audit, a subject request, or a customer's lawyer, and the question is "where has this store been
running since March?". A framework that knew and did not say has made its operator's problem harder
while looking helpful.

**So the dismissal is a first-class, audited act.** An admin acknowledges the mismatch with a reason
they type, over `POST /admin/stores/{store_id}/region-acknowledgement`, behind
`ConsolePermission::ManageStores` and carrying `If-Match` like every other `/admin` write ADR-0094
governs. The reason is required, non-empty after trimming and at most **280 characters**. The body also
names the two country codes the console displayed; if either has moved since that screen was drawn the
write is refused `FailedPrecondition` ([ADR-0096](0096-unprocessable-status.md)) naming both, because
an acknowledgement recorded against a pair nobody was shown is worse than none. The audit action is
`store.edge_placement.acknowledge` on entity `store` — a verb in the last segment, the shape
ADR-0069's `resource.verb` rule and every three-part name in the tree already take.

The durable state is one row: `store_region_acknowledgement`, keyed `(tenant_id, store_id)`, carrying
`profile_country`, `region_country`, the `reason` and `acknowledged_at`, RLS-isolated by `tenant_id` on
the policy shape `config_trees` already uses — `USING (tenant_id = current_setting('app.tenant_id', true))`
([`0004_cloud_config_trees.sql`](../../crates/adapters/store-postgres/migrations/0004_cloud_config_trees.sql));
`store_lease` ([`0051_store_lease.sql`](../../crates/adapters/store-postgres/migrations/0051_store_lease.sql))
carries no policy of its own, in one additive migration
([ADR-0017](0017-migrations.md)) under `crates/adapters/store-postgres/migrations/`, taking the next
free number in the tree when it lands. (Migration numbers are allocated by landing order, never
reserved: ADR-0113 originally named `0052_host_agents.sql`, and 0052 and 0053 were taken by
`edge_placement` and `superseded_generation` before it was written.)

**Nothing clears the row; the comparison does.** An acknowledgement counts as current only while both
stored codes still equal the pair the console reads now. Change the region, or publish a profile with a
different country, and the stored pair no longer matches, so the warning returns unacknowledged — a
different transfer, which the previous person did not agree to — without a second write on any other
route. The store-profile publish never has to know this table exists. The next acknowledgement
overwrites the row, so there is exactly one per store and it is a keyed row rather than a log; the
history of who acknowledged what lives in the audit trail, where history belongs.

The framework's position is not *"do not do this"*. It is *"say that you meant it, in writing, with
your name on it"* — which is the only thing a tool can honestly ask, and is exactly the shape of every
other gate in [`gate-register.md`](../gate-register.md).

**Region routing under `hosted-by-platform` is a scheduling constraint, and it is not this rule.**
[ADR-0113](0113-the-host-agent.md)'s host agents long-poll for jobs in their own region, and *"a job
with no region is returned to nobody"*. That is a match on where a machine is, not a verdict on whether
a transfer is lawful: a store whose region differs from its profile country is routed exactly like one
whose region agrees, warning and all. The mismatch is never an input to the queue, so the two rules
cannot meet.

### The operator is the controller; the framework holds none of the facts that decide this

Stated plainly, because a record that recorded a region and said nothing about its limits would invite
somebody to treat the field as a clearance.

**The framework does not know whether a transfer has a legal basis.** It has no route that could hold
one honestly — a checkbox saying "we have a basis" is a checkbox, not a basis.

**It does not know whether an agreement covers the transfer**, whether an adequacy finding applies,
or whether one lapsed last month.

**It does not know whether an impact assessment was done.** Gate L2 owns a DPIA for *customer
analytics* and nothing wider; no gate in the register today owns an assessment for a hosting transfer,
and this record does not create one.

**It does not know whether the people whose data this is were told.**

The operator who runs a forked `pos_cloud` and its stores decides why this data is processed and how.
They are the controller; this framework is the tool. That is not a legal opinion about anybody's
statute — it is a statement about who holds the facts, and it matches what
[ADR-0076](0076-subject-request-tooling.md) already decided when it made the subject-request tooling
*"the Data Protection contact's instrument, not an autonomous fulfiller"*.

It is worth saying, because it is true and because it is why this mechanism is shaped as it is, that
operators in several of this framework's markets carry duties that attach when data crosses a border.
This record implements none of them. A country code and a label have no rule attached, which is what
makes the mechanism jurisdiction-neutral: it is the same field in all five countries and in the sixth
a fork opens next year. What it gives an operator is a per-store fact, timestamped and attributed,
which is what those duties are discharged against. Gate L3 stops being a memory.

### One line in the fork checklist

[`fork-checklist.md`](../fork-checklist.md) is *"the single list"* of what a fork must supply, and it
gains one line under a new **Hosted edge placements** heading:

> **You choose where a hosted edge placement runs, and you are responsible for what that means.** The
> framework requires a region on every hosted edge placement, records who set it with the acting admin,
> and shows it beside the store's country — including a warning when the two differ. The region label
> and any acknowledgement reason are stored verbatim and read by people: name a place, not a person.
> The framework does not decide whether the transfer is lawful, whether an agreement or consent covers
> it, or whether an impact assessment is needed. Those are yours: gate register **L3**.

One line, in the document a fork actually reads before it deploys, next to the TLS posture and the
per-store secrets. Not a paragraph in an ADR nobody opens twice.

## What this deliberately does not do

- **It does not enforce a residency policy.** There is no rule that a brand's stores must stay in one
  country, no per-tenant allow-list of regions, no refusal derived from either. A policy engine needs
  an authority to author the policy, a version history, a rollback, an override path and an audit of
  the overrides — five mechanisms in service of a rule this framework cannot get right for five
  countries and will not get right for a sixth. A fork that wants residency enforcement builds it on
  top of this field, which is the reason the field is a plain country code rather than an opaque
  token.
- **It does not choose a region.** No default, no inference, and no ranked suggestion either. A
  latency-sorted list of "nearest regions" is a default wearing a hat: the top entry is chosen by
  nearly everybody, and it would be chosen by proximity, which is not the criterion that matters here.
- **It does not classify data for the operator.** It does not assert that an Argon2id PIN hash is
  personal data in their jurisdiction, or that a sole trader's tax code is. ADR-0107 treats the buyer's
  tax code as personal *as a design constraint on this framework*, which is a decision about this
  code and not advice about anyone's law.
- **It does not block an edge placement over a mismatch, and a mismatch never refuses a lease bump.**
  ADR-0110 settled that the lease is the sole authority over which machine is the store, and a legality
  check with a veto over the bump would be a second authority over that question — the failure ADR-0110
  names when it rejects an `edge_placement` field with its own `active` flag: *"Two authorities over
  single-writer is how a system gets two writers: not on the day they are added, but on the day they
  disagree, when one row says one thing and a lease generation says another and nothing in the tree
  states which wins."* The warning is a warning at every layer that renders it.
- **The refusal this record does add is an argument check, and the distinction is not a quibble.** A
  bump that would set a hosted edge placement with no region — or with a malformed one — is refused
  before the transaction opens, `InvalidArgument` naming the field, the same class of refusal a bump
  with an unparseable `store_id` already takes. It says the request is incomplete, not that the move is
  unlawful; it is answered by typing the region, and it cannot be answered by choosing a different one.
  A veto on the *value* is the thing this record refuses to write.
- **It does not tell the edge where it is.** No new config node, no field on the config pull, no
  change to the heartbeat, and `PROTOCOL_VERSION` does not move. The edge's behaviour must not depend
  on its region, because a value the edge can read is a value a domain path can branch on, and
  ADR-0110 is explicit that *"the moment a domain path branches on `edge_placement`, the framework has
  two point-of-sale systems and tests one of them."*
- **It does not verify that the region is true.** For `hosted-by-operator` the region is what an admin
  typed. Nothing here can prove a VPS is in Singapore, and an IP-geolocation lookup would be a guess
  presented as a check — a wrong "verified" is worse than an honest "recorded", because somebody would
  rely on it. Under `hosted-by-platform` the recorded region is one an enrolled host declared and the
  spawn job was filtered on ([ADR-0113](0113-the-host-agent.md)), which is as close as this gets and
  only in mode 3.
- **It does not record where a backup goes.** `RCLONE_REMOTE` is a per-machine value that no workflow
  reads ([`fork-checklist.md`](../fork-checklist.md) §2), and [`backup.sh`](../../deploy/backup.sh)
  ships bytes to a bucket whose location the framework never sees. A store's data can rest in one
  country and its backup in another, and this field says nothing about the second. Naming the gap keeps
  its absence a decision rather than an oversight; closing it needs a second field with a second honest
  answer about who supplies it.
- **It does not record where a device is.** A till pairs from wherever it happens to be, and under
  [ADR-0111](0111-a-second-origin-may-address-the-edge.md) a second origin may address the edge from
  further away still. The region names where the store's data comes to rest, not the path a request
  took to get there.
- **It does not carry a retention period.** That is `retention_days`
  ([ADR-0035](0035-retention-and-pii-masking.md)), chosen per deployment from a country's configured
  value, and gate L2 already owns the decision. How long data is kept and where it is kept are two
  questions, and merging them would let one edit change the other.
- **It does not move gate X3, and it does not propose moving it.** X3 is the cloud's hosting-region
  decision — where a Cell may run — and it stays a single fleet-wide external registration, because
  nothing in this record deploys, moves or splits a `pos_cloud`. L3 is the gate that gains a per-store
  fact. Conflating the two would let a store-level answer look like it had discharged a decision about
  the control plane.
- **It does not model a region as anything comparable below the country.** `region_label` is a string
  the framework never parses, so two edge placements labelled `ap-southeast-1` and `Singapore` do not
  compare equal and the framework never claims they do. ADR-0113's job filter is the one place equality
  on the label matters, and it is exact equality on a value a host itself declared, never a parse. A
  structured sub-country geography would need a taxonomy this repository would then own and keep
  current for five countries, and the only comparison this record actually needs — the mismatch warning
  — is a country-level one, because the country is the coarsest unit any of these duties attaches to.
  If a fork needs finer, it reads its own labels; it should not expect the framework to.

## Consequences

- This record lands on top of ADR-0110's `edge_placement` column and cannot ship before it. Both are
  the same programme and the same branch; every claim below is about the routes and the column that
  record adds, not about routes that exist today.
- The store's edge-placement record gains `region_country` (ISO 3166-1 alpha-2) and `region_label`
  (opaque, non-empty after trimming, at most 64 characters). Both `Option`, both required together on a
  hosted edge placement, both absent on `in-store`. Purely additive: every existing store is
  `EDGE_PLACEMENT_IN_STORE` with no region, which is what those stores are.
- `POST /admin/config/lease/bump` — the only route that writes an edge placement — gains two optional
  request fields and five refusals, each `InvalidArgument` naming its field over
  `api_error_with_details` in [`http.rs`](../../crates/pos-cloud/src/http.rs): a hosted edge placement
  missing either region field; a `region_country` that is not two ASCII letters (`CountryCode::parse`);
  an empty `region_label`; a `region_label` over 64 characters; a region supplied on an `in-store` edge
  placement. No new write route, no new permission identifier.
- One new `/admin` route, `POST /admin/stores/{store_id}/region-acknowledgement`, behind
  `ConsolePermission::ManageStores` with `If-Match`, refusing `FailedPrecondition` when either country
  code has moved since the screen was drawn and `InvalidArgument` on an empty or over-280-character
  reason.
- `crates/adapters/store-postgres/migrations/` gains one additive migration for
  `store_region_acknowledgement` — one row per store, keyed `(tenant_id, store_id)`, carrying
  `profile_country`, `region_country`, `reason` and `acknowledged_at`, RLS-isolated by `tenant_id` on
  the policy shape `config_trees` uses — taking the next free migration number when it lands. Nothing clears the
  row; a changed pair stops matching, and the next acknowledgement overwrites it.
- `/admin/fleet`, `/admin/fleet/{store_id}`, `/admin/stores` and `/admin/stores/{store_id}` each carry
  the two region fields, on the same routes ADR-0110 extends with `edge_placement`.
  `/admin/fleet/{store_id}` and `/admin/stores/{store_id}` additionally gain a `ConfigTreeStore` handle
  on their state — `FleetState` and `RegistryState` hold none today — and do one `load` for the store
  they already name, so they can return the store's country, the mismatch verdict and any current
  acknowledgement. The two list routes gain no config-tree read and render no comparison.
- Three console surfaces gain a column or a badge — Fleet, the store hub, the stores table — and the
  two single-store surfaces gain a warning state, a "country not recorded" state and the acknowledge
  action. The hub needs no new endpoint, per ADR-0099's rule.
- The audit trail gains one action name, `store.edge_placement.acknowledge` on entity `store`, and the
  existing `config.lease.bump` entry's `before`/`after` gain `edge_placement` and both region fields
  beside the generation. Both are written unredacted: the country codes are business metadata, and the
  label and the reason are operator-supplied free text carried whole and deliberately, never logged and
  never in an event payload. The tenant's audit tab therefore answers "where has this store been
  running, since when, and who decided" without a new read.
- ADR-0113's spawn, `stop_and_drain` and `update` jobs each carry this record's `region_country` and
  `region_label` verbatim, and `GET /sync/hosts/{host_id}/jobs` returns only jobs whose region equals
  the region on that host's registry row. A job with no region is returned to nobody: absence is never a
  wildcard, because a wildcard here means "any agent may start this store anywhere", which would arrive
  as a race in mode 3 with no human in the loop. This record's write refusal means no hosted edge
  placement can produce such a job in the first place; the wire rule stands so that no future path can
  make absence mean "anywhere".
- [`gate-register.md`](../gate-register.md) **L3** gains this field as where its answer is recorded, and
  is no longer a fleet-wide note about a fact nobody holds. **X3** is untouched and stays fleet-wide —
  it is about where the cloud may run. Neither gate is cleared by code.
- [`fork-checklist.md`](../fork-checklist.md) gains one line, quoted above.
- [`glossary.md`](../glossary.md) gains **Region**, beside the **Edge placement** entry ADR-0110 adds,
  and says what it is not: not the S3 signing region in `cloud.toml`'s `[artifacts]`, which is
  `"garage"` and *"does not name a geography"*.
- Nothing is removed or renamed, no config node is added, no permission identifier changes, and
  `PROTOCOL_VERSION` does not move. Two columns, one table, five write refusals on an existing route,
  one new `/admin` route, four read fields, one new audit action, one warning, one glossary row and one
  checklist line.
