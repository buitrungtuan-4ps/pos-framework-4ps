# ADR-0114 — Region is a required, recorded, visible attribute of every hosted placement

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Completes [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s hosted modes with the one fact
they create
· Recorded through [ADR-0069](0069-audit-trail.md), behind
[ADR-0067](0067-multi-admin-console-rbac.md)'s `ManageStores` and
[ADR-0094](0094-console-optimistic-concurrency.md)'s `If-Match`
· Leaves fulfilment where [ADR-0076](0076-subject-request-tooling.md) left it — with the operator
· Keeps [ADR-0005](0005-country-neutral-core.md)'s neutrality about countries
· Relates to [ADR-0068](0068-fleet-liveness.md), [ADR-0099](0099-store-hub.md),
[ADR-0070](0070-people-and-access.md), [ADR-0106](0106-the-store-is-a-legal-person.md),
[ADR-0107](0107-the-buyer-is-a-subject.md), [ADR-0035](0035-retention-and-pii-masking.md),
[ADR-0044](0044-fork-and-deploy.md), [ADR-0111](0111-a-second-origin-may-address-the-edge.md),
[ADR-0112](0112-print-agents.md)

Fifth and last of the five records on the **Edge Anywhere** programme. **ADR-0113** (the host tier
and the console's Start button) is still a reserved number with no file, so it is named here in plain
text and not linked — `xtask links` fails a build on a link that does not resolve, and a reserved
number is not a document.

## The problem

### A hosted placement moves a store's personal data onto a machine somebody chose

[ADR-0110](0110-edge-placement-is-a-deployment-axis.md) gives a store one new degree of freedom:
where `pos_edge` runs. What travels with the process is a whole store's operational data, and three
parts of it are personal.

**Employee identifiers and PIN hashes.** The `permissions` node
([ADR-0070](0070-people-and-access.md)) reaches the edge over the config pull and becomes the roster
the box authenticates against. [`config_client.rs`](../../crates/pos-edge/src/config_client.rs)
deserialises each staff member as an id, the code a person types, their granted permissions and
`pin_phc` — the Argon2id PHC string
[`auth.rs`](../../crates/pos-edge/src/auth.rs) verifies against. Those hashes sit on the placement's
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
`placement`, which says *what kind* of machine holds the store and deliberately not where it is.

The word `region` does appear in the tree, and it means something else entirely.
[`config.rs`](../../crates/pos-cloud/src/config.rs) documents `ArtifactsConfig::region` as *"The S3
region name. Garage's own default is `garage`; it is a signing input, not a location"*, and the
default function says it again: *"It participates in the `SigV4` signature and has to match what the
server was configured with; it does not name a geography."* So the only `region` this framework has
today is a string inside a signature.

The store's **country** does exist, in exactly one place and only optionally: `country_code` on the
`store_profile` node ([ADR-0106](0106-the-store-is-a-legal-person.md)), where
[`http.rs`](../../crates/pos-cloud/src/http.rs) uses it to pick the country module that checks the
shape of a tax registration number. A store that never published a profile has no recorded country at
all.

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
it blocks: *"Any hosting region outside it."* **X3** is the hosting-region decision itself.
[ADR-0070](0070-people-and-access.md) says *"a deployment that replicates the cloud across regions
must confirm a DTA/consent first (flagged, not code's call)"*.
[ADR-0076](0076-subject-request-tooling.md) is blunter: the tooling *"records and enables a human
decision; it does not make one"*, and cross-border transfer of an exported payload is *"the
operator's obligation to clear"*.

Every one of those gates is about a fact — **which region** — that nothing in this repository holds.
They are written as fleet-wide decisions because a per-store answer did not exist. ADR-0110 makes the
answer differ per store, which is what turns a flagged gate into a missing column.

## The decision

**Every hosted placement carries a region an admin chose, the trail records who chose it, the console
shows it wherever the store appears, and the framework attaches no rule of its own to the value.**

Mechanism, never policy. What the region *means* — whether the placement is lawful, whether a
transfer is covered, whether anyone was told — is the operator's, and this record says so in the
places an operator will look.

### Region is required on a hosted placement, and `in-store` does not have one

The region is two fields on the store's placement record:

- **`region_country`** — an ISO 3166-1 alpha-2 code, validated with the same `CountryCode::parse` in
  [`locale.rs`](../../crates/pos-proto/src/locale.rs) that the store-profile publish already calls. It
  is a two-ASCII-letter shape check and nothing more, so a fork placing a store in a country it has no
  compiled module for still records the country rather than being refused.
- **`region_label`** — an opaque, non-empty, bounded string the admin supplies: `ap-southeast-1`,
  `Ho Chi Minh City`, `the rack in Sakai`. It is stored verbatim and **never parsed**. It exists so a
  person recognises the place; the framework reads only the country code.

Both are `Option`, both are absent for `in-store`, and both are **required together** when placement
is `hosted-by-operator` or `hosted-by-platform`. The requirement lives in the write, not the column
type, because `in-store` legitimately has no region: the machine is in the shop, and inventing "the
shop's region" would be a second, weaker copy of the address already on `store_profile`.

**A missing region on a hosted placement is a refusal, not a default.** The write answers
`InvalidArgument` naming the field, on the [AIP-193](../naming-and-api.md) envelope every other
`/admin` refusal uses ([ADR-0096](0096-unprocessable-status.md)).

**The set of regions is not closed, and this is the one place the framework differs from its own
habit.** [ADR-0090](0090-tls-postures.md) gave `TLS_MODE` exactly four values and infers nothing from
`DOMAIN`, and that is right: a TLS posture enumerates behaviours the code implements, so a fifth value
would be a behaviour that does not exist. A region names a place in the world. A framework that ships
a closed list of places has decided which places exist, which is the opposite of neutrality and would
be stale the week after a fork opens in a country the list forgot. `hosted-by-platform` still offers a
list — the regions ADR-0113's host tier actually has — but that list belongs to a deployment, not to
this repository.

**A region is required per store, not per fleet, and that is the point.** Gate X3 records one
hosting-region decision for the whole deployment because that was the only shape available. A fleet of
500+ stores across ten brands and five countries does not have one answer; it has five hundred, and
some of them will differ from their neighbours for reasons the operator has and the framework does
not.

### It is recorded in the trail, exactly like every other `/admin` write

Setting or changing a placement's region is an `/admin` write and inherits everything ADR-0110
already gave the placement change: `ConsolePermission::ManageStores`
([`console_rbac.rs`](../../crates/pos-cloud/src/auth/console_rbac.rs)) — the same permission that
bumps the lease ([ADR-0108](0108-the-lease-generation-is-authority.md)), because moving where a
store's data rests and replacing the machine it rests on are the same class of act — `If-Match`
optimistic concurrency ([ADR-0094](0094-console-optimistic-concurrency.md)), and an audit entry.

The entry is `store.placement.update` on entity `store`, with `before` and `after` carrying the
placement and both region fields. [ADR-0069](0069-audit-trail.md) requires that *"a route whose entity
carries personal data must redact it before writing the audit `before`/`after`"* — this one carries
none. A country code and a place name are business metadata, so they go in whole. The actor is the
`AdminContext` snapshot ADR-0069 copies at the moment of the action (`actor_admin_id`, `actor_email`,
`actor_role`), so renaming or removing that admin later never rewrites who chose the region.

**A "current region" column with no history would be wrong**, and ADR-0069 already argued why in
general: a mutable last-changed-by *"answers 'who last touched this' but destroys the history … that
an audit trail exists to keep."* The argument is stronger here than it was there. The interesting
question about a transfer is never only "where is it now". It is "since when, and who decided", and
that question is asked by someone who was not in the room. An append-only entry answers it; a column
cannot.

### It is visible beside the store's country, wherever the store appears

Three surfaces, all of which already exist and none of which needs a new route:

- **Fleet.** `/admin/fleet` and `/admin/fleet/{store_id}` ([ADR-0068](0068-fleet-liveness.md)) carry
  both fields beside the placement ADR-0110 put there, and
  [`Fleet.tsx`](../../dashboard/src/screens/Fleet.tsx) renders them as a column.
- **The store hub.** [ADR-0099](0099-store-hub.md)'s rule is that *"each card reads an endpoint that
  already exists"*, and [`StoreHub.tsx`](../../dashboard/src/screens/StoreHub.tsx) already calls
  `api.fleetStore(tenant, store)` for its Online and Configuration cards. Region rides that read. That
  is what keeps this one screen of work instead of a track.
- **The stores table.** [`Stores.tsx`](../../dashboard/src/screens/Stores.tsx) is where a store is
  created, renamed and archived, so it is where somebody scanning a tenant will see it.

ADR-0110 put placement *"beside liveness rather than on a settings page nobody opens during an
incident"*. Region follows the same rule for the same reason, one step further out: the incident here
is not an outage, it is a question asked in a meeting, and the answer has to be on the screen the
person already has open.

**Where the store's country comes from, and what happens when there is none.** The comparison value is
`store_profile.country_code` ([ADR-0106](0106-the-store-is-a-legal-person.md)), which is optional. When
a store has never published a profile, the console shows **"country not recorded"** beside the region
and makes no comparison. Absence must never render as agreement. Showing the gap is honest, and it
puts the missing profile in front of the one person who can publish it.

### A mismatch warns. It never blocks, and the dismissal is audited

When `store_profile.country_code` and `region_country` are both present and differ, every surface that
shows the region shows a warning against that store. The write still goes through, the placement still
starts, the lease still bumps.

**Blocking would be wrong, and not marginally.** A block is the framework asserting that this
particular cross-border placement is unlawful. Whether it is depends on the basis for the transfer,
whether an agreement or an adequacy finding covers it, whether consent was taken and what was said
when it was — facts held by the operator, under law that changes without a release, in jurisdictions
the framework was never told about. This repository is forked and self-hosted by design
([ADR-0044](0044-fork-and-deploy.md)); a block encodes one country's rule into a tool run by operators
in five, and it is wrong in **both** directions. It refuses placements that are perfectly lawful — a
move between two countries with a standing agreement, a group that has cleared exactly this transfer —
and it catches none of the unlawful ones, because a same-country placement with no lawful basis at all
passes every check a framework could write. A block is a check that fails the honest case and misses
the dishonest one.

And a block that is wrong gets routed around. The admin picks whichever region passes, and the record
then says something false — which is worse than no record, because the next person believes it. **A
dismissed warning is recorded. A dodged block is not.**

**Silence would be worse than either.** ADR-0110's argument about the offline promise applies word for
word: the loss is made visible *before* the event rather than discovered during it. Here the event is
an audit, a subject request, or a customer's lawyer, and the question is "where has this store been
running since March?". A framework that knew and did not say has made its operator's problem harder
while looking helpful.

**So the dismissal is a first-class, audited act.** An admin acknowledges the mismatch with a reason
they type, and that acknowledgement is an `/admin` write behind the same permission, writing
`store.placement.region_ack` with the two country codes and the reason. The framework's position is
not *"do not do this"*. It is *"say that you meant it, in writing, with your name on it"* — which is
the only thing a tool can honestly ask, and is exactly the shape of every other gate in
[`gate-register.md`](../gate-register.md).

The acknowledgement is scoped to the pair it was made about. Change the region, or publish a different
country on the profile, and the warning returns unacknowledged, because that is a different transfer
and the previous person did not agree to it. There is at most one open acknowledgement per store —
`AGENTS.md` §2 forbids an unbounded structure, and this one is a row per store, not a log.

### The operator is the controller; the framework holds none of the facts that decide this

Stated plainly, because a record that recorded a region and said nothing about its limits would invite
somebody to treat the field as a clearance.

**The framework does not know whether a transfer has a legal basis.** It has no route that could hold
one honestly — a checkbox saying "we have a basis" is a checkbox, not a basis.

**It does not know whether an agreement covers the transfer**, whether an adequacy finding applies,
or whether one lapsed last month.

**It does not know whether an impact assessment was done**, and gate L2 already says that is a person's
job.

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
which is what those duties are discharged against. Gates L3 and X3 stop being memories.

### One line in the fork checklist

[`fork-checklist.md`](../fork-checklist.md) is *"the single list"* of what a fork must supply, and it
gains one line under a new **Hosted placements** heading:

> **You choose where a hosted placement runs, and you are responsible for what that means.** The
> framework requires a region on every hosted placement, records who set it with the acting admin, and
> shows it beside the store's country — including a warning when the two differ. It does not decide
> whether the transfer is lawful, whether an agreement or consent covers it, or whether an impact
> assessment is needed. Those are yours: gate register **L3** and **X3**.

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
- **It does not block a placement, and it cannot refuse a lease bump.** ADR-0110 settled that the
  lease is the sole authority over which placement is active, and a region check with a veto would be
  a second authority over that one question — the exact failure ADR-0110 named: *"Two authorities over
  single-writer is how a system gets two writers."* The warning is a warning at every layer, including
  this one.
- **It does not tell the edge where it is.** No new config node, no field on the config pull, no
  change to the heartbeat, and `PROTOCOL_VERSION` does not move. The edge's behaviour must not depend
  on its region, because a value the edge can read is a value a domain path can branch on, and
  ADR-0110 is explicit that *"the moment a domain path branches on placement, the framework has two
  point-of-sale systems and tests one of them."*
- **It does not verify that the region is true.** For `hosted-by-operator` the region is what an admin
  typed. Nothing here can prove a VPS is in Singapore, and an IP-geolocation lookup would be a guess
  presented as a check — a wrong "verified" is worse than an honest "recorded", because somebody would
  rely on it. Under `hosted-by-platform` the recorded region is the one ADR-0113's spawn job carried
  to the agent that started the process, which is as close as this gets and only in mode 3.
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
- **It does not model a region as anything comparable below the country.** `region_label` is a string
  the framework never parses, so two placements labelled `ap-southeast-1` and `Singapore` do not
  compare equal and the framework never claims they do. A structured sub-country geography would need
  a taxonomy this repository would then own and keep current for five countries, and the only
  comparison this record actually needs — the mismatch warning — is a country-level one, because the
  country is the coarsest unit any of these duties attaches to. If a fork needs finer, it reads its own
  labels; it should not expect the framework to.

## Consequences

- The store's placement record gains `region_country` (ISO 3166-1 alpha-2) and `region_label` (opaque,
  non-empty, bounded). Both `Option`, both required together on a hosted placement, both absent on
  `in-store`. Purely additive: every existing store is `in-store` with no region, which is what those
  stores are.
- The placement write gains one refusal — a hosted placement with no region is `InvalidArgument`
  naming the field, over `api_error_with_details` in [`http.rs`](../../crates/pos-cloud/src/http.rs) —
  and one validation, `CountryCode::parse` on the country code.
- `/admin/fleet` and `/admin/fleet/{store_id}` carry two more read fields, on the same routes ADR-0110
  already extended with `placement`. `/admin/stores` and `/admin/stores/{store_id}` carry them too,
  because that is where a store is created.
- Three console surfaces gain a column or a badge — Fleet, the store hub, the stores table — and a
  warning state when the country and the region country differ. The hub needs no new endpoint, per
  ADR-0099's rule.
- The audit trail gains two action names, `store.placement.update` and `store.placement.region_ack`,
  both on entity type `store`, both writing unredacted `before`/`after` because neither carries
  personal data. The tenant's audit tab therefore answers "where has this store been running, since
  when, and who decided" without a new read.
- The mismatch acknowledgement is one row per store, cleared by a change to either country. It is
  state the console reads on every store screen, so it is bounded by construction — `AGENTS.md` §2.
- **ADR-0113's spawn job carries the region, and a host agent may pull only a job whose region is its
  own.** A job with no region is pullable by nobody. The absence is a refusal rather than a wildcard,
  because a wildcard here means "any agent may start this store anywhere", which is the failure this
  whole record exists to prevent — and it would arrive as a race, in mode 3, with no human in the
  loop.
- [`gate-register.md`](../gate-register.md) **L3** gains this field as where its answer is recorded,
  and **X3**'s hosting-region decision becomes per-store rather than a single fleet-wide note. Neither
  gate is cleared by code; both stop being memories.
- [`fork-checklist.md`](../fork-checklist.md) gains one line, quoted above.
- [`glossary.md`](../glossary.md) gains **Region**, beside the **Placement** entry ADR-0110 asks for,
  and says what it is not: not the S3 signing region in `cloud.toml`'s `[artifacts]`, which is
  `"garage"` and *"does not name a geography"*.
- Nothing is removed or renamed, no config node is added, no permission identifier changes, and
  `PROTOCOL_VERSION` does not move. Two columns, four read fields, two audit action names, one
  warning and one checklist line.
