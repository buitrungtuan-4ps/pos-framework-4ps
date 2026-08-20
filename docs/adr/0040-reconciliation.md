# ADR-0040 — Reconciliation is an edge-initiated missing-id diff on the internal surface

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0022](0022-events-partition-strategy.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0031](0031-cloud-adapter-transports.md) · [ADR-0039](0039-config-delivery.md) · `docs/roadmap.md` D10, P7

**Context.** The store's event log is the source of truth; the cloud is a durable copy fed by the
NATS cursor, with the `/internal/ingest` re-push as a backstop ([ADR-0031](0031-cloud-adapter-transports.md)).
`docs/roadmap.md` P7 requires **nightly reconciliation emitting the list of missing IDs to re-push**
so a cloud that dropped events — a broker gap, a retention expiry, a bug — can be told precisely what
to ask for again. D10 lists the reconciliation protocol as an open spec gap; this closes it.

The cloud cannot compute what it is missing on its own. Event ids are ULIDs, time-ordered per store
but **not a dense sequence** ([ADR-0022](0022-events-partition-strategy.md)), so a gap is not
detectable from the cloud's rows alone — there is no "next expected id." Only the edge knows the true
set it emitted. So reconciliation must start from an edge-supplied manifest.

**Decision.**

- **The edge proposes a candidate set; the cloud returns the subset it lacks.** Reconciliation is a
  diff, not a scan: the edge sends the ids it holds for one store over some window, and the cloud
  answers with exactly those the event log does not contain — the ids to re-push. The re-push path
  already exists (`/internal/ingest`, idempotent by event id, [ADR-0026](0026-port-shapes.md)), so
  reconciliation *names* the work and ingest *does* it; a re-pushed id the cloud already has is a
  harmless no-op.

- **It is edge-initiated, matching the outbound-only link.** Like config delivery
  ([ADR-0039](0039-config-delivery.md)), the store drives it — there is no cloud→store channel to
  pull a manifest over ([ADR-0031](0031-cloud-adapter-transports.md)). "Nightly" is the edge's
  cadence; the cloud side is the stateless diff endpoint it calls.

- **It lives on `/internal`, not the public or store-facing surface.** `POST /internal/reconcile`
  takes `{tenant_id, store_id, event_ids: [...]}` and returns `{missing: [...]}`. It sits beside
  `/internal/ingest`: reachable only on the cloud's own private network, unauthenticated at the app
  layer, and absent from the public OpenAPI — reconciliation is an operational fleet mechanism, not
  an integrator API. The diff is scoped by explicit `tenant_id` + `store_id` columns (the same
  columns RLS keys on), so it answers only within one tenant's store.

- **The diff is a set-membership query behind a `ReconcileStore` seam.** The cloud asks the store
  "which of these candidate ids are present for this `(tenant, store)`?" (`store-postgres`:
  `event_id = ANY($candidates)` against the log) and returns the complement. Pure set arithmetic,
  bounded by the candidate page the edge sends, so a reconciliation pass costs one indexed lookup, not
  a table scan.

**Rejected.**

- **Cloud-side gap detection** (infer missing ids from the cloud's own rows) — rejected: ULIDs are
  not a dense sequence, so "what's missing" is unknowable without the edge's true set.
- **Sending whole events in the manifest** — rejected: the events are already in the store's log and
  most are already in the cloud's; sending ids and re-pushing only the genuinely-missing few is far
  cheaper than shipping full envelopes to discover they were duplicates.
- **A background cloud cron that pulls manifests** — rejected: it needs a cloud→store channel the
  architecture does not have. Edge-initiated is the only shape consistent with the outbound-only link.
- **Authenticating `/internal/reconcile`** — rejected for the same reason `/internal/ingest` is
  unauthenticated: `/internal` is a private-network surface, and adding a credential to a
  fleet-internal diff endpoint is scope the network boundary already covers.

**Consequences.**

- **No new `CloudApp` generic.** The endpoint is a small independently-stated sub-router
  (`reconcile_router`) merged into the main router in `main`, with just the `ReconcileStore` as its
  state — so reconciliation does not thread an eighth collaborator through every handler. The seam is
  filled by `store-postgres` (a `PostgresReconcile` query) through the same `persistence.rs` bridge
  pattern as the other cloud seams, and by a fake in the router test.
- **The endpoint is the P7 half; the edge poller is P9.** This lands the cloud's *emit-missing-ids*
  side — the roadmap's named P7 deliverable. The `pos_edge` job that assembles a nightly manifest,
  calls `/internal/reconcile`, and re-pushes the result through `/internal/ingest` is store-side
  fleet wiring (P9), the same split config delivery drew.
- **Proven without the network.** The diff is set arithmetic tested over a fake (missing = candidates
  − present), and the SQL membership query is exercised against a real database in `store-postgres`'s
  gated integration suite.
