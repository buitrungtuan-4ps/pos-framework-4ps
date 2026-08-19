# ADR-0022 — Events partitioning: monthly range on business date, tenant isolation by RLS

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-19
**Relates to** [ADR-0008](0008-postgres-partitioning.md) · [ADR-0016](0016-postgres-access.md) · [ADR-0014](0014-datetime-library.md)
**Resolves** spec-gap: the events partition strategy was ambiguous three ways.

**Context.** [ADR-0008](0008-postgres-partitioning.md) settled *one* partitioned database with RLS
rather than a database per store, but left the partition strategy for the events table
under-specified in three conflicting ways: it says "partition by `store_id`", its only worked naming
example is `events_p_2026_08` (monthly), and its retention line is "archive old partitions". Those
imply three different partition keys — store, month, and time-for-retention. The events table is the
spine of the cloud (every store's every event lands here and rollups read from it), so guessing wrong
is a migration across the largest table in the system.

Three forces:

- **Retention is by time.** `data-protection` keeps events for a bounded window and drops the rest;
  the cheap way to drop a month of data is to `DETACH`/`DROP` a partition, not to `DELETE` rows.
- **Reads are recent-heavy.** Ingest, rollups and dashboards touch the current and previous period far
  more than old data, so partition pruning wants the hot data in its own small partitions.
- **Tenant isolation is a correctness boundary, not a performance one.** "Tenant A must never read
  tenant B" has to hold for *every* query regardless of how the table is physically laid out — so it
  cannot be a property of the partition key.

**Decision.**

- **Range-partition the events table by month on `business_date`** — `events_p_YYYY_MM`. The
  key is the store's **business date** ([ADR-0014](0014-datetime-library.md)), not the server's clock
  time, so a store's trading day lands wholly in one partition regardless of the server's timezone;
  computing a rollup or dropping a month never splits a store's day across two partitions.
- **Tenant isolation is RLS, orthogonal to partitioning.** `tenant_id` is a column on every row with a
  row-level-security policy that filters by the current `app.tenant_id`
  ([ADR-0016](0016-postgres-access.md)); it is **not** part of the partition key. Isolation therefore
  holds identically on every partition, and a query that forgets to set the tenant sees nothing.
  `store_id` is likewise a column and an index, not a partition boundary — a thousand stores would be a
  thousand partitions per month, which is the fragmentation [ADR-0008](0008-postgres-partitioning.md)
  set out to avoid.
- **Retention drops whole partitions.** The masking/retention job (P7) `DETACH`es then `DROP`s a
  monthly partition once it is wholly past the retention window — an `O(1)` metadata operation, no
  per-row `DELETE`, no bloat.
- **Partitions are created ahead of need.** A scheduled step creates next month's partition before it
  is written to, so ingest never races partition creation. The default partition catches anything
  mis-dated and alarms rather than dropping it.
- **The `subject_id` PII side-table follows the same monthly range**, so a buyer's personal data ages
  out on the same schedule as the events that reference it.

**Rejected.**

- **Partition by `store_id`** (ADR-0008's literal words) — rejected: it scatters a time-based retention
  drop across every store's partition and multiplies partition count by the fleet size.
- **Partition by ingest/wall-clock time** — rejected: a store trading past midnight in its own zone
  would have one evening split across two partitions, and rollups computed per business date would read
  two partitions for one day. `business_date` is the axis the queries actually use.

**Consequences.**

- The schema migration (P7) creates the parent table `PARTITION BY RANGE (business_date)` with monthly
  children, the RLS policies on the parent, and a `store_id`/`tenant_id` index inherited by children.
- The rollup tables key on `(tenant_id, store_id, business_date, …)` and are refreshed from the events
  partition for a business date, so a day's rollup reads exactly one partition.
- This supersedes ADR-0008's "partition by `store_id`" phrasing; ADR-0008's decision to keep one
  RLS-guarded database rather than one per store stands unchanged.
