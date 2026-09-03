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

**Scope.**

In: the 12 config-tree saves (shape A) and the 2 collection replaces (shape C).

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
- **Merging concurrent config edits instead of refusing them.** Rejected for now. Two publishes to
  *different* layers could in principle merge, but publishes to the same layer cannot, and a merge
  that silently succeeds sometimes is harder to reason about than a refusal that always explains
  itself. Revisit if the refusal proves noisy in practice.

**Consequences.**

- A config publish can now fail with `412` where it previously always succeeded. The console reloads
  and shows the operator what changed, as it does for every other conditional write in this work.
- Shape C brings the first migration in this line of work: a per-tenant version row for tax rates and
  for translations. ADR-0094's headline property — that `xmin` needs no schema change — holds for
  records and does not extend to collections.
- The config tree gains a second reason to hold `ConfigVersionId` accurately. It was already the
  edge's sync token; it becomes the console's write precondition too. A bug that reuses or reorders
  those ids now costs a wrong refusal or a wrong success, not just a confusing history.
- Six upserts stay last-write-wins until the shape-B decision is made. That is a known, recorded gap,
  not an oversight.

**Delivery.** This ADR, then: shape A as its own slice (12 call sites, one seam, no migration); shape
C as a second slice (2 seams, one additive migration); shape B once the owner has chosen (i) or (ii).
