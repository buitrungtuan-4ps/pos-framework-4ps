# ADR-0017 — Migrations: forward-only, additive, enforced

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-19
**Relates to** [ADR-0015](0015-sqlite-access.md) · [ADR-0007](0007-in-house-vs-dependency.md) · [ADR-0026](0026-port-shapes.md)

**Context.** Both tiers have a schema that changes over releases: SQLite at the edge (P4) and
PostgreSQL in the cloud (P7). The edge is the hard case. A store server is a machine nobody
administers, running on hardware that loses power mid-write, and it may have been **offline for weeks**
before an update reaches it — so it can jump several schema versions at once, and a migration that fails
halfway must leave a database the next boot can still open and finish migrating.

The naming and API rules already forbid removing or renaming a published event type or payload field
([`naming-and-api.md`](../naming-and-api.md) §1, §13), and `cargo xtask snapshot` enforces that for the
event and permission catalogues. The database schema needs the same discipline for the same reason: an
older edge, or a cloud rebuild replaying old events, still expects the columns a shipped migration
created. A migration that drops or renames a column is a break dressed as a change.

**Options considered.**

1. **A migration framework** (`refinery`, `sqlx::migrate!`, `diesel_migrations`). Each bundles a runner
   and a versioning table. Rejected as more machinery than the job needs — the runner is a `for` loop
   over ordered files — and `sqlx::migrate!` in particular drags in the build-time database coupling
   [ADR-0015](0015-sqlite-access.md) rejected. None of them enforces *additive-only*, the property that
   actually matters here.
2. **An ORM that diffs models and generates migrations.** Rejected outright: generated destructive
   migrations are exactly the failure mode, and an ORM is a large dependency in the write path of a
   database [ADR-0007](0007-in-house-vs-dependency.md) says to keep thin.
3. **Plain ordered SQL files, a tiny in-house runner, and a CI gate that enforces additive-only.**
   Chosen.

**Decision.** Migrations are **numbered, ordered SQL files** committed in the repository, named
`NNNN_snake_case_description.sql`. A tiny runner — a loop, not a framework — applies every file whose
number is greater than the database's current version, each inside its own transaction, recording it in
a `schema_migrations(version, applied_time)` table. The runner is **forward-only**: there are no `down`
migrations, because the rollback story is restoring a backup ([`architecture.md`](../architecture.md)
§4), never running reverse SQL against a store that may already have written data under the new shape.

**Additive-only is enforced, not merely documented**, by a `cargo xtask migrations` check that fails a
pull request when:

- a migration file that already exists on the base branch has been **edited** (a shipped migration is
  immutable — the same removal-gate principle `xtask snapshot` uses for the catalogues); or
- a new migration contains a **destructive statement** — `DROP TABLE`, `DROP COLUMN`,
  `ALTER … RENAME`, or a narrowing type change — outside an explicitly annotated, reviewed escape hatch.

A column is retired the way an event field is: it stays, it is marked deprecated in a comment, and it
stops being written; it is dropped only after two releases and a changelog upgrade note, never in the
migration that stops using it. Adding a column uses a default or is nullable, so applying it to a
populated table needs no backfill lock.

The identical rule governs both tiers. The edge runner and its `schema_migrations` table land in P4
with `store-sqlite`; the cloud runner over PostgreSQL lands in P7 with `store-postgres` (ADR-0016),
sharing the file-naming convention, the immutability gate, and the destructive-statement gate.
Tier-specific SQL lives in tier-specific directories, so a migration is never assumed to be portable
between SQLite and PostgreSQL.

**Consequences.**

- The migration "tool" is a few dozen lines plus a CI check, with no framework dependency in the write
  path — consistent with [ADR-0007](0007-in-house-vs-dependency.md).
- A schema change that would break an offline edge fails CI as a visible diff, the same way a removed
  event field does, rather than surfacing in production when a two-week-old store finally reconnects.
- A migration mid-failure leaves `schema_migrations` one row behind, so the next boot resumes at the
  first unapplied file — the multi-version jump is just the loop running more iterations.
- Cost: a genuinely necessary destructive change is deliberately awkward (an annotated escape hatch, a
  two-release deprecation). That friction is the point; the default must be safe.
- The additive-only guarantee is what lets [ADR-0026](0026-port-shapes.md)'s "the outbox commits with
  the state change" stay true across versions — the outbox table's columns can only ever grow.
