# `crates/pos-cloud` — local rules

Read the root [`AGENTS.md`](../../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** the cloud binary. Event ingest, rollups, the configuration tree, the `/v1`
integrator API, the `/admin` console API and the `/sync` surface stores call. See
[`README.md`](README.md).

## Local rules

- The cloud never dials into a store. A store reaches it on `/sync/*`.
- Ingest is idempotent by event id. A replayed batch changes nothing.
- Every `/admin` route checks a `ConsolePermission`, declared in `console_permissions!` in
  `crates/pos-cloud-api/src/auth/console_rbac.rs`
  ([ADR-0067](../../docs/adr/0067-multi-admin-console-rbac.md)). Every `/admin` route is either in
  `src/openapi_admin.rs` or named in its `UNDOCUMENTED` list, which only shrinks.
- `docs/openapi.json` and `docs/openapi-admin.json` are generated:
  `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi`. Never edit them by hand.
- A stored table change is a new migration in `crates/adapters/store-postgres/migrations/`, and a
  shipped migration is never edited ([ADR-0017](../../docs/adr/0017-migrations.md)).
- Two crates sit beside this one. [`pos-cloud-api`](../pos-cloud-api/README.md) holds what every
  route stands on: authentication, `ConsolePermission`, the audit trail, paging and the error
  envelope; it has no routes. [`pos-cloud-reports`](../pos-cloud-reports/README.md) holds the
  rollups, revenue, the X/Z report, their CSV exports and their routes, which `pos-cloud` mounts.
  A report changes there, not here.
- `src/http.rs` is very large, and its areas are moving into `src/http/<area>` one pull request at
  a time (plan row 5). Add a handler beside its area's router, in the area's module once it has
  one, and do not move code in a feature PR: moves are their own pull requests (ADR-0170
  decision 6). An area's route tests move the same way, into `tests/cloud/<area>.rs`.

## Most often touched

`src/http.rs` · `tests/cloud.rs` · `src/persistence.rs` · `src/main.rs` · `src/openapi_admin.rs`

## Gates

`cargo test -p pos-cloud` (including the OpenAPI drift tests) · the `postgres` job · the
`console-replay` job · `xtask migrations` ·
`changes/<topic>.md` for a user-visible change (`docs-gate`, [`changes/README.md`](../../changes/README.md)).

## Read first

The [repository map](../../docs/repository-map.md) section on cloud routes ·
[`docs/architecture.md`](../../docs/architecture.md) §3 ·
[ADR-0033](../../docs/adr/0033-config-tree.md) · [ADR-0019](../../docs/adr/0019-openapi-generation.md).
