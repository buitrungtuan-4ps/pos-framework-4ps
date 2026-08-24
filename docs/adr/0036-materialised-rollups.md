# ADR-0036 — Dashboards answer from a materialised rollup, maintained by a projector cursor

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0016](0016-postgres-access.md) · [ADR-0022](0022-events-partition-strategy.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0031](0031-cloud-adapter-transports.md) · `docs/roadmap.md` P7

**Context.** The event log is the cloud's source of truth, and `Cloud::daily_rollups` already computes
a store's per-trading-day activity from it correctly. But it does so by scanning the whole log every
call — O(events) — and the P7 exit criterion is explicit that **dashboards answer from rollups under
10 ms**. At a store's lifetime scale that scan is far too slow, so the rollup a dashboard reads must
be **materialised**: precomputed and kept current, so a view is a small lookup, not a full replay.

**Decision.**

- **Maintain the rollup with a projector cursor over the log.** [`dashboard::project`] reads the
  events after a stored cursor, folds each into the store's running per-day totals, and advances the
  cursor. The cursor moves forward only, so **every event is folded exactly once** — the projection
  is idempotent (a pass with no new events changes nothing) and incremental (a later pass folds only
  what arrived since). This is the same "cursor over the log" shape as the ingest consumer and the
  webhook feed ([ADR-0031](0031-cloud-adapter-transports.md)); resetting the stored cursor and
  re-projecting **rebuilds the rollup from the log** (the roadmap's reset-cursor-and-replay).

- **One fold, shared, so the two paths cannot diverge.** The from-log `Cloud::daily_rollups` and the
  materialised projector both call the *same* `fold_event`. The materialised rollup is therefore, by
  construction, whatever a full re-scan would compute; a test asserts the equality directly
  (materialised `dashboard()` == authoritative `daily_rollups()` over the same events). Refactoring
  `daily_rollups` onto the shared fold changed no behaviour — its existing rollup test still passes.

- **The dashboard read cannot touch the log — by its type.** [`dashboard::dashboard`] takes a
  `RollupStore` and **no `EventStore`**: it answers purely from the materialised store, oldest day
  first. "Answers from rollups, not the log" is thus a fact of the signature, not a discipline, and
  the O(days) lookup is what delivers the sub-10 ms answer. A dedicated test reads the dashboard with
  no event store in scope at all.

- **Eventually consistent, deliberately.** The materialised rollup trails ingest by a projection
  cycle — near-real-time, which is what a dashboard needs. The log remains the source of truth; the
  rollup is a cache that is always reconstructible from it.

**Rejected.**

- **Computing from the log on every view** (today's `daily_rollups`) — kept as the *verifiable*
  reference and reused by tests, but rejected as the dashboard's read path: it is the O(events) scan
  the exit criterion rules out.
- **Folding at ingest time, keyed on newly-appended events** — rejected: `append` reports how many
  events were new but not *which*, so at-least-once redelivery would double-count. The projector
  cursor sidesteps this entirely by folding the durable log, each event once, independent of how
  ingest batched or retried.
- **A second, separately-written rollup fold for the materialised path** — rejected: two folds would
  drift. One shared `fold_event` makes agreement structural.

**Consequences.**

- No new dependencies. The engine — the fold, the projector, and the dashboard read — is pure and
  I/O-free behind a `RollupStore` seam, and unit-tested with no database: the materialised rollup
  equals the from-log computation, projection is idempotent and incremental, and the dashboard reads
  only the materialised store.
- **Deliberately not here yet:** the `store-postgres` materialised-rollup table implementing
  `RollupStore` ([ADR-0016](0016-postgres-access.md)), the background projector task that runs
  `project` as events ingest (alongside the NATS cursor), and pointing the public
  `/v1/stores/{store_id}/rollups/daily` route at `dashboard()` instead of the from-log
  `daily_rollups` (the response shape is identical, so the OpenAPI does not change). Those are the
  wiring; this slice is the read model and its correctness.
- Money- and order-shaped rollups beyond the activity counts, and the eight-item dashboard backlog
  the archive enumerates, extend the same materialised model with more folds.
