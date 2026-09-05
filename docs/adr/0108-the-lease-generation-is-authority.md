# ADR-0108 — The lease generation is authority, and a box takes it once

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Completes [ADR-0049](0049-single-active-lease.md) · Delivered over
[ADR-0052](0052-ota-rollout-config.md)'s config rail and
[ADR-0068](0068-fleet-liveness.md)'s heartbeat
· Relates to [ADR-0003](0003-cattle-not-pets.md), [ADR-0055](0055-edge-ota-updater.md),
[ADR-0094](0094-console-optimistic-concurrency.md)

## The problem

[ADR-0049](0049-single-active-lease.md) is the record that makes **"one store, one active
machine"** true across a swap. It built `lease_standing(held, authoritative)` as a pure function of
two generations with no clock, so that a store cut off from the cloud keeps selling and a machine
stops being active only when a newer generation is deliberately issued.

**Neither generation exists anywhere.** `lease` appears nowhere in `pos-cloud`: no table, no seam,
no route, nothing that could increment a counter. It appears nowhere in `pos-edge`'s production
code either. The one call site is the OTA tick, which passes a literal and says so:

```rust
// `Active` because nothing on the edge learns its lease standing yet: ADR-0049's generation
// lives in the cloud and no published node carries it.
match self.updater.run(&device, &plan, LeaseStanding::Active).await
```

The comment is honest about the edge and wrong about the cloud. So the highest-level safety
property in `docs/roadmap.md` P9 — the one that stops a replaced machine acting as the store — is
a pure function with no arguments, and `pos-core::lease` has **zero production callers**.

This is not merely unimplemented. `docs/roadmap.md` D10 lists "lease protocol details" among the
things that must be **"resolved in the phase that needs it — not silently invented at
implementation time"**, and ADR-0049 §Consequences deferred *persisting the authoritative
generation* to `Fiscalization` in P10. There is a decision to make before there is code to write.

### Why the deferral has outlived its reason

ADR-0049 bundled two things into that deferral: **allocating a real invoice range** and
**persisting the generation**. The bundle is why neither has been built.

Allocating a legal invoice range is a `Fiscalization` call to a tax authority. Whether that call
can be made at all is a **legal-registration question, answered per country and not by this
repository**. Persisting a monotonic counter needs a table and an increment.

Bundling the second behind the first means the half that stops two machines selling as one store
waits on the half that needs a government's permission. **They are separated here.** The generation
lands now; the invoice range stays exactly where ADR-0049 put it.

## The decision

**The cloud owns a per-store lease generation as authority — a counter it may only bump. The store
takes its generation once, on first sight, and thereafter only compares.**

### The generation is authority, so it is a table and not an authored node

`docs/adr/0004-cloud-owned-configuration.md` puts all configuration in the cloud's config tree, and
the obvious move is to make the lease one more node an operator publishes. That would be wrong for
two reasons a config tree is *supposed* to have:

- **A config node is authored.** Something a person types into a form is not an authority; a typo
  in the generation field would promote or demote a machine.
- **A config tree rolls back.** Version history and rollback shipped with the audit trail
  ([ADR-0094](0094-console-optimistic-concurrency.md), G2). A generation that a rollback can move
  *backwards* is not monotonic, and monotonic is the entire mechanism.

So the authoritative generation is `store_lease` in Postgres, one row per store, and the only write
the cloud offers is **bump**. There is no set-to-a-value route and no decrement — an authority that
accepts a number from its caller is not one.

### It reaches the store as a derived `lease` node on the Store layer

The config pull is the only rail that reaches a store, and building a second one to carry one
integer would be worse than reusing this one. So the bump writes the table **and** publishes
`{"generation": N}` as a Store-layer `lease` node in the same action, and no admin route accepts a
`lease` node body: the node is derived from the table, never the other way round.

It is its own node, not a field inside `device_ota` ([ADR-0052](0052-ota-rollout-config.md)).
`device_ota` decides **which release a store takes**; `lease` decides **whether this box is the
store**. Different authors — one an operator choosing a ring, the other never a person — different
lifecycles, and merging them would let a placement edit touch the lease.

### The box takes its generation once, enforced by the schema

This is the load-bearing rule, and without it the whole mechanism is decorative: a superseded box
pulls the next config, reads generation `N+1`, adopts it as its own, and calls itself `Active`
again — the supersession lasting exactly until the next config pull.

So the held generation is **durable, edge-local, and written once**: a `store_lease` row on the
store's own SQLite, inserted with `ON CONFLICT DO NOTHING`. The rule lives in the schema rather than
only in the Rust that happens to call it, because the Rust is one refactor away from an `UPDATE`
and the schema is not.

It has to be durable for the same reason [`0006_ota_state.sql`](../../crates/adapters/store-sqlite/migrations/0006_ota_state.sql)
does: an install **deliberately restarts the edge** ([ADR-0055](0055-edge-ota-updater.md)). A held
generation in process memory would be re-adopted from config on every boot, which is the decorative
version with extra steps.

### A rollback cannot promote anybody

Config rollback still exists, and it can move the *node* backwards even though it cannot move the
table. Take-once makes that safe in the direction that matters. If the published generation rolls
back below what a box holds, `lease_standing` returns **`Invalid`**, not `Active` — and every box
in the store reads `Invalid`, because they all hold something at or above the rolled-back value.

A rollback therefore makes the store refuse, never promotes the wrong machine. That is the correct
failure direction for a mechanism whose whole purpose is to stop two machines believing they are
one store.

### A store with no lease behaves exactly as it does today

No `lease` node published means no authoritative generation, which means the box is weighed as it
is now — eligible. This is not a gap left open; it is what makes the change deployable to a fleet in
which no store has ever been issued a lease. The refusal begins the first time a store is issued
one, which is a deliberate act by a named admin, recorded in the audit trail.

The `lease` node follows the config tree's **never-blank rule**: a document that omits it, or
carries one that does not parse, leaves the previously-applied value alone. A malformed publish must
not silently un-supersede a machine.

### The store reports the generation it holds, so the console can see a split

The refusal is invisible from the cloud unless the box says what it holds. The heartbeat
([ADR-0068](0068-fleet-liveness.md)) already carries `outbox_depth` in an optional JSON body that
an older edge may omit entirely; the held generation joins it there, `Option<u64>`, absent on a box
that holds none.

That is what makes a **split** legible: the console can put "this box holds 3, the store's
authority is 4" in front of an operator, which is the difference between a fleet screen that shows
a stale box and one that shows a *replaced* box. It is also the missing half of the lease recovery
action `production-readiness.md` **W4** named as blocked on exactly this row.

## What this deliberately does not do

- **It does not stop a superseded box selling.** `Superseded` refuses an *install*, and that is
  all. Read-only selling is ADR-0049's P9e assignment and touches every write path in
  `pos-edge::app`, with a far worse failure mode if it fires wrongly — a shop that cannot take
  money is a worse outcome than a shop running last week's binary. It wants its own slice, its own
  operator-visible refusal, and its own way back. Naming it here keeps its absence a decision.
- **It does not allocate an invoice range.** `LeaseGrant::invoice_range` and `issue_replacement`
  stay unwired, waiting on `Fiscalization` exactly as ADR-0049 said. The cloud stores the
  generation only. When the range lands, it extends this row; it does not replace it.
- **It does not put the generation on the `LeaseToken`.** ADR-0049 rejected deciding standing by
  comparing opaque tokens, and that stands: the token is the credential the edge presents, the
  generation is the order.
- **It does not reconcile a box whose held generation is `Invalid` back to the authority.** There
  is no automatic recovery, deliberately — a box ahead of its authority means corruption or a
  restored backup, and the answer to that is a person looking at it, not a silent re-adoption of
  whatever the cloud last said.

## Consequences

- `pos-core::lease` gets its first production caller, eleven phases after it was written and
  property-tested.
- The cloud gains a `LeaseStore` seam, `store_lease` in Postgres, and one route: bump this store's
  lease. It sits behind `ConsolePermission::ManageStores` — replacing a store's machine is a
  store-management act, not an OTA one — and writes an audit entry naming the generation it moved to.
- The edge gains a `LeaseAuthority` in the durable edge-local category `OtaStateAuthority`,
  `QueueNumberAuthority` and `ReceiptAuthority` already occupy: a `pos-edge` trait implemented for
  `SqliteStore` over its public API, with an in-memory twin held to the same expectations. It is
  **not** a port — nothing swaps in a different one, and no vendor sits behind a store's memory of
  which lease it holds.
- The OTA tick reads a real standing. A store whose lease cannot be read is **not** weighed as
  active: it takes the same refusal `StateUnreadable` already takes, because weighing an unreadable
  lease as `Active` is the failure this record exists to remove.
- `production-readiness.md` **R4** closes, and **W4**'s lease half is unblocked.
- One integer is added to the heartbeat body and one node to the Store layer. Both are additive,
  both are optional, and no `PROTOCOL_VERSION` moves.
