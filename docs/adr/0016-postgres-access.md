# ADR-0016 — Cloud PostgreSQL access: `tokio-postgres` behind a pool, SQL by hand

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-19
**Relates to** [ADR-0008](0008-postgres-partitioning.md) · [ADR-0015](0015-sqlite-access.md) · [ADR-0013](0013-async-strategy.md) · [ADR-0007](0007-in-house-vs-dependency.md)

**Context.** `pos_cloud` (P7) reads and writes PostgreSQL — the partitioned, RLS-guarded event and
rollup store ([ADR-0008](0008-postgres-partitioning.md)). Rust has two mature ways to talk to it, and
the choice touches every build, not only the cloud one:

- **`sqlx`** — `async`, and its macros check every query against a real schema *at compile time*. That
  safety is real, but it needs either a live database reachable during `cargo build`, or a committed
  offline cache (`.sqlx/`) that must be regenerated against a database whenever a query changes.
- **`tokio-postgres`** — `async`, no compile-time query checking; queries are strings the pool
  prepares at runtime, and correctness is proven by tests against a real database.

Two constraints decide it.

- **The repository must build on a fresh checkout with no services** — that is true today (every CI
  job compiles the workspace with no database) and it is the promise `fork-and-deploy` (P8) makes to a
  contributor. `sqlx`'s compile-time checking would push a build-time database or an offline cache
  onto every fork; a stale `.sqlx/` becomes a confusing build break unrelated to the change in hand.
- **The edge already chose hand-written SQL behind a narrow gate** ([ADR-0015](0015-sqlite-access.md):
  `rusqlite`, a dedicated writer thread, no macro-checked queries). One SQL discipline across both
  tiers is easier to review than two.

**Decision.** The cloud speaks to PostgreSQL through **`tokio-postgres`**, with a **`deadpool-postgres`**
connection pool, and **SQL written by hand** in the `store-postgres` adapter. Specifically:

- **No compile-time query checking, no build-time database.** The workspace compiles with nothing
  running. Query correctness is a **test** obligation, not a compiler one — see below.
- **One pool per process**, sized to the cloud's concurrency envelope; every query runs on a pooled
  connection with a statement timeout.
- **Tenant isolation is RLS, set per transaction.** Each request opens a transaction and issues
  `SET LOCAL app.tenant_id = $1` (and `SET LOCAL ROLE app_tenant`), so the row-level-security policies
  ([ADR-0008](0008-postgres-partitioning.md)) filter every statement by the current tenant. A query
  that forgets the `SET LOCAL` sees **nothing**, not everything — the policies default-deny.
- **Migrations are forward-only and additive**, run by the same `xtask` discipline as the edge
  ([ADR-0017](0017-migrations.md)), against the cloud schema.

**Why the compiler's help is not worth the cost here.** `sqlx`'s check catches a column typo at build
time; an integration test against a real PostgreSQL catches the same typo *and* the RLS bug, the
partition-routing bug, and the idempotency bug that no compiler can see. The cloud's correctness lives
in behaviour the type system cannot describe — that a second ingest of the same ULID writes nothing,
that tenant A cannot read tenant B, that a dashboard answers from a rollup — so the test suite is not
optional either way, and once it exists the compile-time check is a smaller marginal gain than the
build-time-database cost it imposes on every fork.

**How correctness is proven.** `store-postgres` runs the shared port contract suites
([ADR-0026](0026-port-shapes.md)) **against a real PostgreSQL**, plus cloud-specific tests for RLS and
idempotency. A test connects to `DATABASE_URL`; when it is unset the test **skips with a notice**, so
the PR `test` job stays fast and database-free while the merge-to-`main` `integration` job — and a
developer with `DATABASE_URL` set — runs the real thing against a pinned `postgres:16` service
container. A suite that only ever ran against a fake would prove nothing about the real store, which
is the whole point of [ADR-0026](0026-port-shapes.md)'s harness.

**Consequences.**

- `tokio-postgres`, `deadpool-postgres` and `postgres-types` join the cloud dependency surface (not the
  backbone). `cargo-deny` reviews them like any other dependency.
- Hand-written SQL means a typo can reach a test rather than the compiler; the integration suite is the
  net, and it is required to merge cloud code.
- A future move to `sqlx` is not foreclosed — the adapter hides SQL behind the port, so the query layer
  can change without the domain noticing — but it would have to carry an offline cache to keep the
  no-build-time-database property, which is the cost this decision declines to pay now.
