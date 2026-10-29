# `pos-cloud-people`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

The cloud's people and access:
- employees, role templates and the assignments that give a person a role at a store, a store group or the whole tenant;
- the compile of a store's people to its `permissions` node, its publish, and the queue that republishes a store when what it compiles from changes;
- whether each role a store runs has someone to fill it;
- the capability flags;
- their routes and stores.

It stands on `pos-cloud-api` (authentication, the audit trail, task health and the refusals), `pos-cloud-config` (the publish every area writes through, the registry, settings, store groups and the tenant layout) and `pos-cloud-catalog` (the employee list refuses page shaping as the item list does). `pos-cloud` re-exports `people`, `people_compiler`, `people_readiness` and `people_republish` under their old paths, imports `routes` into its `http` module as `people`, and runs the republish loop.

## Owns

- Employees, role templates and assignments, and the `EmployeeStore`, `RoleTemplateStore` and `AssignmentStore` traits
- The permission catalogue, and the compile of a store's people to its `permissions` node
- The republish queue, its drain loop and the `PeopleRepublishStore` trait
- A store's permissions readiness
- The capability catalogue and a store's capability flags
- Their routes:
  - `/admin/employees/*`, `/admin/roles/*` and `/admin/assignments/*`;
  - `/admin/people/*` and `/admin/capabilities`;
  - `/admin/config/capabilities`;
  - `/admin/stores/{store_id}/permissions/readiness`.
- Its stores over PostgreSQL, through `store-postgres`

## Depends on / used by

- **Depends on** (workspace crates): `pos-cloud-api`, `pos-cloud-catalog`, `pos-cloud-config`, `pos-core`, `pos-ports`, `pos-proto`, `store-postgres`.
- **Used by** (workspace crates): `pos-cloud`.

## Entry points

- `src/people.rs`: the records, the three store traits and the permission catalogue
- `src/people_compiler.rs`: `compile_permissions`; `src/people_readiness.rs`: `store_readiness`
- `src/people_republish.rs`: `PeopleRepublishStore`, `drain`, `run`
- `src/routes.rs`: `people_router`; `src/routes/`: `people_publish_router`, `capabilities_router`, `config_capabilities_router`, `permissions_readiness_router`, `permissions_nodes`, `republish_people`
- `src/persistence.rs`: the store-postgres impls

## Rules

- [ADR-0070](../../docs/adr/0070-people-and-access.md): people and access
- [ADR-0071](../../docs/adr/0071-config-without-json.md): the capability flags
- [ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md): the till enforces each person's own permissions
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4 and [ADR-0175](../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md): why this is a crate

## How to test

```bash
cargo test -p pos-cloud-people
```

The routes' HTTP tests drive the whole router, so they stay in `crates/pos-cloud/tests/`, with the store contract suites.
