# ADR-0101 — The cloud stamps an ingested event's tenant; the store does not claim one

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Implements production-readiness **S2**
· Extends [ADR-0003](0003-cattle-not-pets.md), [ADR-0016](0016-postgres-access.md)
· Relates to [ADR-0089](0089-edge-event-bus-transport.md), [ADR-0097](0097-internal-route-authentication.md)

## The problem

Every event a store publishes carries `tenant_id` and `brand_id` in its envelope, and every store in
the fleet stamps the same two values: `StoreIdentity::for_store` hard-codes `ULID(1)` for both, and
`main.rs` is its only production caller. The doc comment says activation supplies the real ones. It
does not — `ActivationGrant` carries a device id and a credential and nothing else, and the edge's
activation handler records only the device id.

So the column that row-level tenant isolation is defined on holds one constant for the whole fleet.
Three things follow, and only the first is visible today:

1. **The `events` RLS policy separates nothing, and switching it on would invert into a leak.**
   `0001_cloud_events.sql` defines `USING (tenant_id = current_setting('app.tenant_id', true))`, and
   nothing in production sets `app.tenant_id` or assumes the `app_tenant` role — the adapter connects
   as the owner, which bypasses a policy that is not `FORCE`d. If the posture ADR-0016 describes is
   ever wired up, a real tenant sets its real id and sees **none** of its own events, while anything
   whose tenant is `ULID(1)` reads the whole fleet's raw log.
2. **Reconciliation would loop for ever.** `absent_event_ids` filters
   `WHERE tenant_id = $1 AND store_id = $2`, with `$1` taken from the request body. A caller passing a
   real tenant matches nothing against rows stamped `ULID(1)`, so the cloud answers "I am missing
   every one of these" and the store re-pushes its whole window, permanently. `/internal/reconcile`
   has no non-test caller yet (ADR-0097), so this is armed rather than firing — and it is the thing
   that breaks first when it gets one.
3. **`brand_id` is a lie in the log.** Nothing in `pos-cloud` reads it off an envelope today, so it
   costs nothing now and would attribute the entire fleet to one brand the moment anything did.

Everything *else* is already safe, and that is the clue to the fix: dashboards, `/v1` rollups, fleet
liveness, webhooks, alerts, audit and retention all key on a **server-derived** tenant — the store
registry (`active_stores`) or the verified API-key grant. The event log is the one place that trusts
the caller's claim.

## The decision

**The cloud stamps `tenant_id` and `brand_id` onto every ingested event from its own store registry,
overwriting whatever the envelope claims. The edge stops pretending to know them.**

`Cloud::ingest` is the single funnel both paths pass through — the NATS cursor and
`POST /internal/ingest` — so the stamp goes there, before `EventStore::append`. The lookup is the
existing `StoreDirectory` seam, widened from `tenant_of` to an `owner_of` that answers the tenant and
the brand together from one row of the `stores` registry.

This is the shape the tree has already chosen twice. `/v1` reads take the tenant from the grant and
say so in a comment at the line. The OTA report was deliberately moved off `/internal` *because*
"that route read `tenant_id` and `store_id` out of the body, so it believed the caller's claim about
which store it was". The event log is the last route that still believes it.

### What happens to an event from a store the registry does not know

**It is stored with the tenant it claimed, and a warning is logged.** Not dropped, and not fatal.

The alternatives are worse in both directions. Refusing the batch would let one message from an
unknown store block the whole fleet's ingest behind a cursor that never acks. Dropping the event
would lose a real store's trading history to a missing registry row — a provisioning bug becoming
data loss. A store that publishes has a registry row by construction (the console creates it before
the wizard writes a `config.toml`), so this branch is a diagnostic, not a path.

### What this does **not** close

A caller holding the fleet's NATS credential can still publish events naming **another store's** id,
and the stamp will then file them under that store's real tenant. This ADR narrows the claim from
"any tenant" to "any store in the fleet", which is a strict improvement and is not a fix for the
underlying problem: the broker's subject is fleet-wide and its credential is fleet-wide.
[ADR-0089](0089-edge-event-bus-transport.md) already names per-store mTLS as the path there, and this
decision neither helps nor hinders it.

## Why not have the edge learn its tenant

The obvious alternative is to put `tenant_id` and `brand_id` in `config.toml` — the provisioning
wizard already knows both, and today writes the tenant into a *comment* the parser discards. It was
rejected for two reasons:

- **It keeps the trust where the problem is.** A value the edge asserts is a value the cloud has to
  either believe or check; if it checks, the config entry was never needed. The registry is already
  the authority for which tenant owns a store — the projector and every admin route read it — and a
  second copy on a box is a second copy to drift.
- **A typo becomes silent mis-attribution.** `EdgeConfig` is `deny_unknown_fields`, so a *missing*
  key is loud; a *wrong* ULID is not, and would file a store's entire trading history under the wrong
  tenant with nothing in the system disagreeing.

The edge therefore keeps a placeholder, renamed to say what it is: `StoreIdentity::UNASSIGNED` for
both ids, the nil ULID rather than `ULID(1)`. A nil id that reaches a report reads as "nobody", which
is the truth; `ULID(1)` reads as a tenant that might exist.

## Consequences

- No migration. `tenant_id` plays no part in the events table's idempotency key
  (`PRIMARY KEY (business_date, event_id)`), so correcting the column cannot affect dedup. Existing
  rows keep whatever they hold; no production tenant exists to backfill.
- `StoreDirectory` gains `owner_of`. `tenant_of` stays — the order relay uses it and asks a narrower
  question.
- The `events_tenant_store_date` index stops having a constant leading column.
- Reconciliation's tenant filter starts matching, which is the precondition for **R3** ever working.
- ADR-0016's `SET LOCAL app.tenant_id` posture becomes implementable. Implementing it is still its own
  piece of work and is not part of this decision.
