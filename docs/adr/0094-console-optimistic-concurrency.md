# ADR-0094 — The console stops losing edits: an opaque version at the seam, Postgres `xmin` beneath it

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-02
**Relates to** [ADR-0016](0016-postgres-access.md) (the adapter this lands in) · [ADR-0017](0017-migrations.md) (the migration this deliberately does not need) · [ADR-0033](0033-config-tree.md) (the whole-document save that is the worst case) · [ADR-0060](0060-cloud-back-office-dashboard.md) (the console that will send the header) · [ADR-0065](0065-cloud-org-registry.md) · [ADR-0066](0066-cloud-catalog.md) (the two entity families that go first) · [ADR-0069](0069-audit-trail.md) (the trail that records the winner but not the loser) · `docs/naming-and-api.md` §4 (the canonical status list this adds to) · `docs/roadmap-v3.md` **Q3c**

**Context.** **Every master-data edit in the console is last-write-wins, silently.**

Two managers open the same menu item. One changes the price, saves. The other — holding the form
they loaded thirty seconds earlier — changes the name and saves. The `PATCH` body carries *every*
field, so the second save writes the stale price back over the first. Both admins see success. The
audit trail ([ADR-0069](0069-audit-trail.md)) faithfully records two updates and gives no hint that
one erased the other, because from the store's point of view nothing went wrong: it was asked to set
a price, and it set it.

The measurement, taken across `crates/pos-cloud/src/http.rs` and the `store-postgres` adapter:

- **24 mutating `/admin` routes** — 15 `PATCH` and 9 `PUT` — carry no concurrency control of any
  kind.
- **Nothing in the tree sends or reads `ETag`, `If-Match` or `If-None-Match`**: not the cloud, not
  the console client, not the edge. This is a greenfield mechanism, not a repair.
- The worst case is not an item, it is the **config tree**. `ConfigTreeStore::save` persists a
  store's whole four-layer state as one JSON document, "replacing any prior one"
  ([ADR-0033](0033-config-tree.md)). Two admins publishing *different nodes* concurrently — one
  editing channels, one editing tax — read the same document and each writes back their own version
  of the whole thing. The second publish silently discards the first admin's node, not just a field.

**A correction to the record.** An earlier scoping pass of this work reported that `updated_at` was
unmaintained on the catalog tables and that "21 of 35" `UPDATE` statements failed to set it. Both
figures were wrong. Re-measured: **38 `UPDATE` statements, 23 of which set `updated_at`, 15 of which
do not** — and every one of the 15 is a *state transition* (revoke a key, resolve an alert, accept
an invite, mask a subject, cancel a scheduled publish), not a master-data edit. The master-data
writes this ADR is about, `set_item` included, do maintain it. The reason to reject `updated_at` as
the version is therefore different from — and weaker than — the one first given, and it is set out
under *Alternatives* below rather than quietly dropped.

**What the codebase already gets right, and why it is the model.** Those 15 transition writes are
not careless. Each one guards on the state it expects, in the `WHERE` clause:

```sql
UPDATE api_keys           SET revoked = true      WHERE id = $1 AND revoked = false
UPDATE alerts             SET resolved_at = $2    WHERE id = $1 AND resolved_at IS NULL
UPDATE scheduled_publishes SET status = 'CANCELLED' WHERE tenant_id = $1 AND id = $2 AND status = 'PENDING'
```

That *is* optimistic concurrency — a compare-and-swap in one atomic statement, where a second
revoke changes zero rows and the caller can tell. The tree already knows the technique and applies
it wherever the expected prior state is a single flag. What is missing is the same discipline for a
master-data edit, where the expected prior state is *the whole record* and cannot be spelled out as
a predicate. This ADR gives that case a predicate: the row's version.

**Decision.**

**1. A version is an opaque token, minted by the adapter, never interpreted above it.**

`pos-cloud` gains `Version(String)` and `Versioned<T> { record: T, version: Version }`. A read that
can be written back returns `Versioned<T>`; a write takes the version the caller believes it is
replacing. The seam contract is one sentence: *an update given an expected version applies only if
the stored version equals it, and does so atomically.* Nothing above the adapter may parse, compare
for ordering, or construct a token — only echo it back.

This is what keeps the framework forkable, and it is the direct answer to the cost accepted in
choosing `xmin`: **the Postgres tie lives in one adapter, not in the seam, the HTTP layer, or the
console.**

**2. Beneath it, the `store-postgres` adapter uses the `xmin` system column.**

Postgres stamps every row with the transaction that last wrote it, and changes it on every `UPDATE`.
So the compare-and-swap is the statement itself:

```sql
UPDATE catalog_items
   SET name = $3, …, updated_at = now()
 WHERE tenant_id = $1 AND menu_item_id = $2 AND xmin = $4::text::xid
RETURNING xmin::text
```

One statement, no extra round trip on the happy path, no lock held across the caller's think-time,
and **no migration** — the column already exists on every table in the schema, including tables a
fork adds tomorrow without knowing this ADR exists.

Zero rows back means the update did not apply, but not *why*. The adapter then probes
`SELECT xmin::text FROM … WHERE id = $1`: absent is `NotFound`, present is `VersionMismatch`. One
extra query, only on the failure path. A row deleted in the gap between the two answers `NotFound`,
which is the truthful answer to "why did my update not apply".

The seam returns that three-way outcome rather than today's `bool`:

```rust
enum UpdateOutcome { Updated(Version), VersionMismatch, NotFound }
```

**3. On the wire: a strong `ETag`, an `If-Match` that is required, and `412` when it does not match.**

- A read-one route answers `ETag: "<token>"`. A **list** route cannot use a header — one response,
  many rows — so each row carries the same token as an additive `etag` JSON field
  (`#[serde(flatten)]` over `Versioned<T>`, so no existing field moves or is renamed). The two forms
  carry **byte-identical** strings, so a client never reformats and the two can never disagree.
- A mutating route requires `If-Match`. Absent is a `400` `INVALID_ARGUMENT` with
  `details: [{ "field": "if-match", "reason": "REQUIRED" }]` — the same shape Q3b gave every other
  missing input, rather than a tenth status for a case that is an ordinary missing field.
- A mismatch is **`412`**.

**4. `ErrorStatus` gains one variant, `VERSION_MISMATCH` → `412`.**

The envelope maps to 400/401/403/404/409/429/500/503 and nothing else
(`pos_proto::error::ErrorStatus::http_code`), so `412` is not expressible today. `FAILED_PRECONDITION`
already means "the system is in the wrong state for this request" and already maps to `409`, which is
what a second `bill:settle` must return; overloading it would make two genuinely different answers
indistinguishable to a client and would give the wrong HTTP code for a conditional request.

Adding a variant is safe by construction and for the reason the module was built that way: `status`
is `Open<ErrorStatus>`, so a client built before this ADR reads `VERSION_MISMATCH` as unrecognised
and still gets an intact `code`, `message` and `details` instead of a parse failure. Per the standing
decision that `/v1` error bodies may change shape freely while no external consumer exists, this does
not move `pos-api-version`.

**It is named `VERSION_MISMATCH`, not `PRECONDITION_FAILED`.** A canonical-looking
`PRECONDITION_FAILED` (412) sitting beside the existing `FAILED_PRECONDITION` (409) is two tokens
differing only in word order with different HTTP codes — a defect waiting to be written by someone
reading quickly. `VERSION_MISMATCH` says what happened and cannot be confused with anything.
`docs/naming-and-api.md` §4 records it alongside the canonical nine, as the second documented
deviation from AIP-193 after `UNSPECIFIED`.

**5. Two corrections to the shape this was approved in, both in the same direction.**

The option accepted for the version basis previewed `ETag: W/"xmin-1847302"`. The shipped form is
`ETag: "1847302"`:

- **No `W/`.** RFC 9110 §13.1.1 evaluates `If-Match` with the *strong* comparison function, under
  which a weak validator never matches. A weak `ETag` would make every conditional write fail — the
  opposite of the intent.
- **No `xmin-` prefix.** Naming the mechanism in the token contradicts decision 1: it would put
  "this is Postgres" on the wire and in every client, which is exactly the fork cost the opaque-token
  seam exists to contain.

Neither changes the decision that was taken — `xmin` is still the basis — only the spelling of the
token it produces.

**Scope, and what it deliberately excludes.**

In: the 15 `PATCH` routes and the `PUT` routes that *replace a record in place* (tax rates,
campaigns, ingredients, recipes, suppliers, translations), plus the config tree's whole-document
save, which gets its own slice because a clobber there costs a whole node rather than a field.

Out, with reasons:

- **Create routes.** There is no prior version to match. Duplicate-create is a different problem, and
  `idempotency-key` (`docs/naming-and-api.md` §4) is its answer.
- **The 15 transition writes.** They already carry a compare-and-swap predicate, as shown above.
  Requiring `If-Match` as well would add a round trip and a failure mode to writes that are already
  safe.
- **`/v1` and `/sync`.** The store-facing and edge-facing surfaces are append-only or
  idempotent-by-key; neither has a read-modify-write cycle with a human thinking in the middle,
  which is the condition this ADR addresses.
- **`If-None-Match` and conditional *reads*.** Bandwidth, not correctness. The console's payloads are
  small and the cache-header work already shipped. Flagged, not started.

**Alternatives.**

- **`updated_at` as the version.** Rejected, on the corrected measurement. It is a *convention*: 15
  of 38 writes already do not set it, nothing in the schema enforces it — no trigger, no generated
  column — and the next `UPDATE` someone writes without `updated_at = now()` would silently disarm
  the guard for that route, with no test able to notice. A concurrency primitive that a future edit
  can forget is worse than none, because it fails open: a stale `If-Match` would *match* and the
  clobber would proceed with the client believing it was protected. It would also need adding to
  every `SELECT` (no read returns it today), so it is the same work as `xmin` without the safety.
- **A `version bigint` column, bumped by a trigger.** Portable to any SQL store, and honest. Rejected
  for cost and reach: a migration across every admin table plus a trigger per table, repeated by
  every fork that adds one — against `xmin`, which is already on every row including a fork's new
  ones. It remains the recommended path for a fork that leaves Postgres, and the seam is shaped so
  that swapping to it changes one adapter.
- **A content hash of the row.** No schema change and fully portable, but it must be computed on
  every read and every write, it is sensitive to serialisation order, and two writes that set a
  field back to its previous value produce the same hash — so it cannot distinguish "unchanged" from
  "changed and changed back", which is precisely a lost-update window.
- **Pessimistic locking (`SELECT … FOR UPDATE`).** Correct and simple in the database, and wrong for
  this shape: the lock would have to be held across a human editing a form. A row locked by an admin
  who went to lunch blocks the store.

**Consequences.**

- **A `PATCH` without `If-Match` stops working.** This is a breaking change to `/admin`, taken
  deliberately: the alternative — accepting an absent header as "no opinion" — leaves exactly the
  silent-clobber path this ADR exists to close, and leaves it as the *default*. `/admin` has one
  consumer, the console in this repository, which is updated in the same track.
- **The console must handle `412`** by telling the admin their copy is stale and offering to reload —
  not by retrying, which would re-apply the clobber through a different door.
- **The cost the fork inherits is one adapter method per entity**, not a scheme. A fork on another
  store implements `Version` minting however its engine allows; nothing above the seam changes.
- **No migration, no protocol-version bump, no event change.** `xmin` is already there, the `etag`
  JSON field is additive, and `VERSION_MISMATCH` degrades in an older client by construction.
- **Not a distributed-transaction guarantee.** It protects a single row (or, for the config tree, a
  single document) against a lost update. Two edits to *different* rows that are only meaningful
  together are still two writes, and this ADR does not make them one.

**Delivery.** This ADR, then: the mechanism proven end-to-end on the registry's four routes; the
catalog's eleven; the remaining families (floor, people, inventory, campaigns, tax, translations);
the config tree's whole-document save; and the console — `If-Match` on every write, a stale-copy
recovery on `412`. Each is a slice with its own tests, in that order.
