# ADR-0008 — Partitioned PostgreSQL with RLS

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** An early design gave each store its own database. At a thousand stores this means a thousand migrations, a thousand backups and a thousand ways to drift.

**Decision.** One PostgreSQL instance; tables partitioned by `store_id`; row-level security for tenant isolation; JSONB for adapter-specific payloads; rollup tables for dashboards. Cross-store and cross-brand queries stay in SQL.

**Consequences.**
- Operations scale with instances (one), not with stores.
- Sub-10 ms dashboards without a separate analytics engine.
- Disk is the only linear wall; see the sizing formulas in `capacity-and-reliability.md`.
- Cross-tenant leakage is prevented by RLS, which must therefore be covered by tests, not by convention.
