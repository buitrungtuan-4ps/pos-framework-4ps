# ADR-0095 — What ADR-0094 left: three shapes, not one, and only one of them is hard

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-03
**Amends** [ADR-0094](0094-console-optimistic-concurrency.md) — splits its remaining scope into three
shapes, decides two of them, and corrects its claim that `If-None-Match` is a bandwidth question.
**Relates to** [ADR-0033](0033-config-tree.md) (the tree whose version this reuses) ·
[ADR-0066](0066-cloud-catalog.md) (the two keyed upserts deferred from slice 3) ·
[ADR-0069](0069-audit-trail.md) · [ADR-0074](0074-localization-and-tax.md) ·
[ADR-0079](0079-inventory-and-suppliers.md) · `docs/roadmap-v3.md` **Q3c**

**Context.** [ADR-0094](0094-console-optimistic-concurrency.md) put the console's record-shaped
entities on conditional writes: an opaque `Version` at the seam, Postgres `xmin` beneath it, a strong
`ETag` on the wire, `If-Match` required, `412 VERSION_MISMATCH` when it does not match. Three slices
delivered it — the registry's four entities, the catalog's nine, and the floor and people families'
five — and the mechanism carried across all eighteen without a special case.

What it did **not** deliver is the rest of its own scope, which it described in one line as "the `PUT`
routes that *replace a record in place* (tax rates, campaigns, ingredients, recipes, suppliers,
translations), plus the config tree's whole-document save". That line reads as one homogeneous group.
Measured on the seams, it is three, and they need three different answers:

| | Seams | What it is | Prior version at a first write? |
|---|---|---|---|
| **A** | `ConfigTreeStore::save` | a document that **already has** a domain version | yes, or a checkable "none" |
| **B** | 6 keyed upserts | create-or-replace **at a key** | **no** |
| **C** | `set_tax_rates`, `TranslationStore::save` | a genuine **whole-collection** replace | no, but the collection always exists |

The measurement behind that table, taken across `crates/pos-cloud/src/http.rs` and the seams:

- **12 `ConfigTreeStore::save` call sites**, not one. Every one reconstructs the tree from a freshly
  loaded `ConfigTreeState` (`ConfigTree::from_state`, 16 uses across the read and write routes; 20
  `load(tenant_id, store_id)` calls in total) and then replaces the whole four-layer document plus its
  history. Two concurrent publishes read the same prior state; the second `save` erases the first's
  layer edit **and** its history entry. Nothing on that path sends, reads, or compares a version.
- **6 keyed upserts**, not the eight this work had booked as wholesale replaces: `upsert_ingredient`,
  `upsert_recipe`, `upsert_supplier`, `upsert_campaign`, `set_placement`, `set_layout_button`. Their
  own doc comments say what they are — "creates X, or replaces the one that already has its id".
  Each sits beside a `list`/`get` and a `delete`. They are not collection replaces at all.
- **2 genuine collection replaces**: `set_tax_rates` (the tenant's whole rate table) and
  `TranslationStore::save` (the tenant's whole grid). Both tenant-scoped, both read whole by a
  matching `list`/`load`.
- **`RoutingRuleStore` and `AssignmentStore` need nothing**: create/list/remove, no update at all.

**A correction to ADR-0094's record.** ADR-0094 excluded `If-None-Match` as "bandwidth, not
correctness. … Flagged, not started." That is true for conditional *reads*, which is what it had in
view. It is **false for shape B**: there, the create-versus-overwrite distinction *is* the
correctness question, and `If-Match` alone cannot express it. The exclusion stands for reads and is
withdrawn for keyed upserts.

**A second correction, to this work's own earlier framing.** An earlier scoping note for this slice
said the answer was "a version on the collection, keyed by tenant or tenant+store", applied to all
eight remaining seams. On the measurement that answer fits **two** of them. The config tree already
holds a better one, and the six upserts need a different mechanism entirely.

**Decision.**

**1. Shape A — the config tree writes conditionally on the `ConfigVersionId` it already has.**

`ConfigTree::current_version() -> Option<ConfigVersionId>` exists today. That id is already minted on
every publish, already carried to the edge, already recorded in the audit trail, and already listed
by the version-history routes the Config screen renders. `ConfigTreeStore::record_store_seen` already
takes a `held_version` of the same type. So the config tree needs **no new token, no new column and
no new concept**: `If-Match` carries the `ConfigVersionId` the tree was read at, and the write applies
only if it still equals `current_version()`.

This is strictly better than an opaque `xmin` token *here*, and only here:

- **It is meaningful to the operator.** A `412` on a config publish can say which version is now
  current, and the history screen already shows what that version changed. An opaque token can only
  say "something moved".
- **The never-published case is expressible.** `current_version()` returning `None` is a real,
  checkable state — the tree exists but has no published version — so a first publish is
  distinguishable from a stale one without a second header. This is exactly what shape B lacks.
- **It costs nothing to keep.** The value is already computed, stored, and surfaced.

It does **not** generalise. Every other entity in this work is versioned by the adapter under
ADR-0094's rule that the seam never interprets a token. The config tree is the one place where a
*domain* version already exists and is already the thing an operator reasons about, and reusing it is
therefore a deliberate local exception, recorded here rather than left to be discovered.

> **Amended.** "`If-Match` carries the `ConfigVersionId`" holds for **2 of the 12** call sites, not
> all 12. See *Correction, on implementation* below, written after building it.

**2. Shape C — a version on the collection, keyed by tenant.**

`set_tax_rates` and the translation grid each replace one tenant-scoped collection. Neither handler
reads before it writes: the read-modify-write cycle happens in the **console** — the screen loads the
whole grid, an operator edits one cell, the screen `PUT`s the whole grid back. That is precisely the
condition ADR-0094 names: a read-modify-write with a human thinking in the middle. The lost update is
no less real for spanning the browser; it is more real, because the thinking takes minutes.

The collection is the entity. `list_tax_rates`/`load` returns it with a `Version`; the write takes
the `Version` it was read at, exactly as ADR-0094's record-shaped writes do. Under `store-postgres`
the version cannot be a row's `xmin`, because the collection is many rows and a replace deletes and
reinserts them; it is a version row keyed by tenant, bumped in the same transaction as the replace.
That is one small additive migration, and ADR-0094's "no migration needed" property does not survive
into this shape — which is a cost worth naming rather than eliding.

> **Amended.** One migration, not two, and there is a third write in this shape that neither refuses
> nor needs one. See *Correction, on implementation: shape C* below, written after building it.

**3. Shape B — not decided here.**

Six keyed upserts cannot be made conditional by `If-Match` alone. At a first write there is no prior
version to name, so a request without `If-Match` is either "create this" or "overwrite whatever is
there, blind", and the server cannot tell which the caller meant. RFC 9110 §13.1.2's answer is
`If-None-Match: *`, which asserts "only if it does not exist yet". Two shapes are then available:

- **(i) Keep the upsert, take two headers.** `If-None-Match: *` to create, `If-Match: "<version>"` to
  replace, neither accepted as a bare write. Smallest diff; the seam keeps one method. But it puts a
  second conditional header on the wire for one family of routes, and a caller that sends neither
  gets a refusal whose message has to explain a distinction the route's own name denies.
- **(ii) Split the seam into create and update.** `create_*` returns `409 ALREADY_EXISTS` on a
  duplicate key; `update_*` takes `If-Match` like every other write in this work. Larger diff — six
  seams, their adapters, their fakes and their console callers — but the resulting API is the one the
  rest of `/admin` already has, and the header story stops being special-cased.

**Recommendation: (ii).** An upsert is a convenience that hides which of two very different things
the caller is doing, and every mechanism bolted onto it has to re-derive that distinction at runtime.
Splitting it states the intent in the method name, where a reader can see it. The cost is a wider
diff, not a harder problem.

**This is the owner's call**, because it changes six route shapes rather than adding a header to
them, and because (i) is genuinely cheaper if the appetite for churn is low. Recorded here so the
choice is made deliberately; the implementing slice does not start until it is made.

**Correction, on implementation: a layer is a map of nodes, and different keys commute.**

Written after the slice was built. The decision above treats a config layer as a *document*, so that
any two publishes of the same layer contend and the second must be refused. That is what the twelve
call sites look like from the seam. It is not what they are.

A layer is a JSON **object keyed by node** — `floor`, `stations`, `inventory`, `campaigns`, `tax`,
`locale`, `channels`, `tender`, `qr`, `permissions`, `capabilities`. Ten of the twelve saves are
*node publishes*: a route composes one or two named keys from data the operator authored on some
other screen — the ingredient rows, the area and table rows, the campaign rows — and writes only
those keys, leaving the rest of the layer as it found it. Two of the twelve are *authored layer
writes*: `PUT /admin/stores/{store_id}/config/{level}`, where an operator types the JSON document
itself, and `POST /admin/stores/{store_id}/config/rollback`, which replaces the current document with
an earlier one.

That distinction decides the mechanism, and it cuts the other way from the decision above:

- **Node publishes take the storage precondition with a retry, and no `If-Match`.** Publishing the
  menu and publishing the floor plan touch disjoint keys. They commute: whichever order they land
  in, both survive, and neither operator has lost anything. Refusing the second one would show
  someone "the configuration changed, try again" for an edit that never touched their key — friction
  with no correctness behind it, on ten routes. What these writes need is the precondition at the
  *store*, so the loser of a race notices, reloads, re-applies **its own keys** to the winner's
  layer, and saves again. Both nodes land; nobody sees a conflict. Three attempts, then a `412`,
  because a caller that loses three times is contending with something other than a human.
- **The two authored writes take `If-Match`, exactly as decided above.** Here the operator *is* the
  document. A second writer really does destroy typed work, and there is nothing to merge: the
  request carries a whole layer composed by a person, not a delta the server can re-apply. The
  wildcard `If-Match: *` remains how a caller asserts "this store has never been published to" — the
  one place in this work a wildcard is accepted, because here it means *only* if nothing is there,
  the opposite of what it means on a record.

Two properties make the retry safe rather than a quiet last-write-wins:

1. **The write is a delta.** `publish_config_nodes` takes `Vec<(String, Value)>` — the keys this
   route owns — and re-applies them to whatever layer it just re-read. It never recomposes the whole
   layer from a stale read, which is precisely the bug it would otherwise reintroduce.
2. **Two publishes of the *same* node produce the same node.** A node is composed from stored
   authored rows, not from request state, so two operators who both press "publish inventory"
   compose the same document from the same rows. Same-key contention exists but has no lost update
   in it.

The background activator for scheduled publishes ([ADR-0077](0077-campaigns-and-scheduling.md)) takes the same
storage precondition. It has no operator and no header, but it still composes on what it read, so
losing the race leaves the row pending and the next pass retries it — a delay, not an error.

What this does **not** change: the domain check still exists on the two authored routes and still
compares `ConfigVersionId`, because an operator can read it. What it adds is the second check
underneath, at the store, on **every** save — and that is the one that prevents the interleave. A
check against your own stale read is not a check; it only reports what the handler already saw.

**Correction, on implementation: shape C is one migration and three writes, not two of each.**

Written after the slice was built. Measuring the two seams against the schema they actually sit on
changed two things this record had asserted.

**The translation grid needs no migration.** This ADR said shape C "brings the first migration in
this line of work: a per-tenant version row for tax rates **and** for translations". That is right
for one of them. `catalog_tax_rates` (0028) is many rows per tenant and a save *replaces* them —
`DELETE` then `INSERT` — so every row's `xmin` is destroyed and there is nothing left that a later
caller's token could be compared against. It needs migration 0039: a version row whose own `xmin` is
the token, claimed first inside the same transaction as the replace.

`translations` (0008) is **one `jsonb` row per tenant**, keyed by `tenant_id`. A save updates that
row in place, so its own `xmin` moves and versions the grid for free. Structurally it is the config
tree of shape A, not a collection at all — what makes it *read* like a collection is the console
screen, not the storage. The reasoning that put it in shape C was about the read-modify-write
spanning a browser, which is still right and is why it takes `If-Match`; the inference that it
therefore needed a version table was not.

The general form, worth keeping: **whether a version needs a home of its own is a question about the
write, not about the entity.** A write that updates rows in place can use their `xmin`. A write that
deletes and reinserts cannot, whatever the thing above it is called.

**There is a third write in this shape, and it retries.** This record counted two: `set_tax_rates`
and `TranslationStore::save`. `save` has two callers. `PUT /admin/translations` replaces a grid an
operator typed cell by cell, and takes `If-Match` as decided. `POST /admin/translations/import/apply`
does not: it parses a CSV, **merges** those keys into whatever grid is stored, and leaves every other
key alone. That is a delta, and it is the same shape as the config node publishes — the rows to apply
are in the request, so losing a race is a stale read rather than a lost update, and the fix is to
re-read and re-apply rather than to refuse. It retries: three attempts, then `412`.

So shape C is **two refusals and one retry**, over one migration. The rule that separates refusing
from retrying is the one the shape-A correction states — a write that *replaces a document a human
composed* refuses; a write that *applies its own keys* retries — and it cuts across the three shapes
rather than along them.

**Scope.**

In: the 12 config-tree saves (shape A) and the collection replaces (shape C). Shape A landed as 10
retried node publishes + 2 `If-Match` authored writes, and shape C as 2 `If-Match` replaces + 1
retried CSV merge, per the corrections above.

Out, with reasons:

- **The six keyed upserts.** Blocked on the decision above, not on the mechanism.
- **`If-None-Match` on conditional *reads*.** ADR-0094's exclusion stands: bandwidth, not correctness.
- **`RoutingRuleStore` and `AssignmentStore`.** No update method exists to make conditional.
- **The 15 transition writes and the create routes.** Excluded by ADR-0094 for reasons that are
  unchanged.

**Alternatives.**

- **One collection version for all eight remaining seams.** This work's own earlier plan. Rejected on
  the measurement: it fits two of them, ignores a better answer already sitting in the config tree,
  and does not address the create-versus-overwrite question at all.
- **An `xmin` version row for the config tree too, for uniformity.** Rejected. It would add a second,
  opaque version alongside the `ConfigVersionId` the operator already sees, and a `412` would then
  have to explain which of two versions moved. Uniformity is worth less than an operator being able
  to read the conflict.
- **Serialising config publishes behind a per-store lock.** Rejected. It converts a lost update into a
  wait, holds a lock across an operator's thinking time if taken at read, and if taken only at write
  does not prevent the clobber it is meant to prevent.
- **Merging concurrent config edits instead of refusing them.** Rejected as stated, and then
  *partly adopted* on implementation. The rejection assumed the unit of contention is a layer. It is
  a node, and disjoint nodes need no merge logic at all — re-applying your own keys to a re-read
  layer is not a merge, it is a retry. Where a genuine merge would be needed — two people typing the
  same layer — the refusal stands.

**Consequences.**

- The two authored config writes can now fail with `412` where they previously always succeeded. The
  console reloads and shows the operator what changed, as it does for every other conditional write
  in this work. The ten node publishes keep their existing contract: they retry and succeed, and only
  a caller that loses three races in a row sees a refusal.
- Shape C brings the first migration in this line of work: a per-tenant version row for tax rates
  (0039). ADR-0094's headline property — that `xmin` needs no schema change — holds wherever a write
  updates rows in place, which turned out to include the translation grid and to exclude only the
  table whose save deletes and reinserts every row.
- The config tree gains a second reason to hold `ConfigVersionId` accurately. It was already the
  edge's sync token; it becomes the console's write precondition too. A bug that reuses or reorders
  those ids now costs a wrong refusal or a wrong success, not just a confusing history.
- Six upserts stay last-write-wins until the shape-B decision is made. That is a known, recorded gap,
  not an oversight.

**Delivery.** This ADR, then: shape A as its own slice (12 call sites, one seam, no migration); shape
C as a second slice (2 seams, one additive migration); shape B once the owner has chosen (i) or (ii).

Shape A is delivered. `ConfigTreeStore::load` returns a `Versioned<ConfigTreeState>` and `save` takes
the version it was read at; `store-postgres` compares `xmin` and distinguishes a missing row from a
stale one; the ten node publishes go through one retrying helper rather than twelve hand-rolled
load→publish→save cycles; the two authored writes and the scheduled activator take the precondition
directly. The correction above is the reason the shape differs from what this ADR first decided.

Shape C is delivered. `TaxRateStore::list_tax_rates` returns the grid and its version and
`set_tax_rates` takes the version it was read at, over migration 0039; `TranslationStore::load`
returns a `Versioned<TranslationGrid>` over the row's own `xmin`, with no migration. The version
rides on the read's `ETag` and comes back as `If-Match`, because a tax-rate body is a JSON array with
nowhere to carry a field. `If-Match: *` asserts "nothing saved here yet" and is refused once a
version exists. The CSV import retries.

Shape B remains blocked on the owner's choice of (i) or (ii).
