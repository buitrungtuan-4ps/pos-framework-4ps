# ADR-0189 — A store runs the package version HQ pins

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0033](0033-config-tree.md) (what the published document carries),
[ADR-0066](0066-cloud-catalog.md) (a second authored source compiled into it) and
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md) (what a store may
choose) · Relates to [ADR-0039](0039-config-delivery.md), [ADR-0078](0078-sync-and-ota-closure.md),
[ADR-0122](0122-a-store-group-is-a-delivery-cohort.md),
[ADR-0195](0195-the-cloud-wakes-a-store-to-pull-its-config.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

The owner wants HQ to design screens, flows, rules and data, and stores to run what HQ picks: locked
to one version, or choosing among versions HQ allows, never editing. Six dimensions can make what a
till shows differ: brand, store, device, role, sales channel and time slot.

What exists:

- **The config tree** ([ADR-0033](0033-config-tree.md)) publishes one validated document per store,
  delivered as a delta or a snapshot by pull, applied with last-known-good, and rolled back by
  publishing again.
- **Authoring is separate from publishing.** The catalog is a normalized store the cloud compiles
  into flat nodes (`menu`, `layout`) at publish time ([ADR-0066](0066-cloud-catalog.md)).
  Settings are typed fields authored at tenant, brand, group or store scope, most specific wins,
  compiled at write time ([ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md)).
- **The cloud knows each store's release** ([ADR-0078](0078-sync-and-ota-closure.md)), and already
  hides a setting a release does not honour (ADR-0160 item 5).
- **Nothing is versioned as a unit.** A screen is hand-written in `ui/`; a layout is one node among
  twenty. A store cannot be pinned to "the March screens", nor rolled back to them.

## Options considered

1. **More nodes in the store's tree, authored per scope.** No unit to pin, compare or roll back;
   a screen and the flow it calls could be published half-way.
2. **A package outside the config tree, with its own delivery.** A second path that can disagree with
   the first, which [ADR-0062](0062-the-relay-wake.md) refused for orders on the same grounds.
3. **A versioned package authored in its own store, pinned by a setting, compiled into the published
   document** (chosen). It reuses deltas, last-known-good, rollback and the push of ADR-0195.

## Decision

1. **A package is a tenant's versioned bundle of four parts:** screens
   (ADR-0190), flows (ADR-0192), rules (ADR-0193) and data definitions
   (ADR-0194). It is identified by `package_id`; each published version by `package_version_id` (a
   ULID) with a `version_label` people read. A published version never changes. Authoring lives in a
   tenant-scoped, RLS-isolated package store, as the catalog does; the tree carries only compiled
   output. A package names prices, taxes, promotions and reason codes by id and never carries them.
2. **The pin is a setting.** A new typed `package` node, authored at tenant, brand, group or store
   scope like any ADR-0160 setting:
   - `mode`: `PACKAGE_MODE_UNSPECIFIED` (no pin), `PACKAGE_MODE_LOCKED` or `PACKAGE_MODE_CHOOSE`;
   - `locked_version_id` for a lock; `allowed_version_ids` and `default_version_id` for a choice;
   - `chosen_version_id`, written only at store scope, only from the allowed list, by a console user
     who holds the new permission `admin.package.choose` for that store. A store's people choose;
     they never edit.
   The most specific scope wins, so **brand and store are resolved here, in the cloud.** A lock at a
   broader scope cannot be loosened below it once lock by path (roadmap-v3 B7.2) lands.
3. **The version rides the published document.** At compile time the cloud writes the pinned
   version's content into the store's document as the `package` node's `content`, with its
   `package_version_id`. It is delivered, applied, retained for *K* versions and rolled back exactly
   as any node. A compiled version is at most 1 MiB; a larger one is refused at publish.
4. **Device, role, sales channel and time slot are resolved on the store**, because they change
   within one store's day. Any screen, flow or rule may carry **variants**, each with a selector that
   states zero to four of these dimensions:
   - a variant is a candidate when every dimension it states matches (device by `device_id`, else by
     device class; role by role id; channel by `SalesChannel`; time slot by a named slot from the
     package's `time_slots`, evaluated in the store's time zone through `ClockSource`);
   - the candidate stating the most dimensions wins; a tie goes by the fixed order device, role,
     sales channel, time slot;
   - every part has a base variant with no selector, and two variants with the same selector are
     refused at publish, so resolution always answers and always answers the same.
   The till and the edge each implement this function once, and both run one shared file of test
   vectors. A slot that changes takes effect at the next screen entry; a flow in progress keeps its
   variant (ADR-0192).
5. **A minimum edge version per package version.** At publish the cloud computes
   `min_edge_version`: the highest release that introduced any component, action or rule function
   the version uses, read from the catalogues each release declares (ADR-0190, ADR-0191). Nobody
   types it. The cloud compiles a version into a store's document only if the store's reported
   release meets it; otherwise the store keeps the version it runs, and the console shows that
   store as waiting for an edge update. The edge checks again when it applies, and refuses a version
   above its own release as a whole.
6. **Last-known-good.** The edge validates a package on apply: the schema, every reference resolves,
   every rule compiles within its limits. A failure keeps the whole previous document
   ([ADR-0033](0033-config-tree.md)) and is reported in the heartbeat. **Rollback** is pinning the
   previous version, which is a new publish delivered like any other.
7. **No pin means the default package.** Each release carries a default package: today's screens,
   once phase 2 expresses them as one. A store with no `package` node runs it, so upgrading changes
   nothing until HQ pins a version (ADR-0160 item 1).

## Consequences accepted

- **Additions to `pos-proto`:** the `package` node and the package content types, additive, with the
  owner's review. The settings register gains the node's fields; `admin.package.choose` joins the
  permission snapshot.
- **A new cloud store** for packages, by an additive migration, behind a seam with a fake.
- **Switching versions is a large delta.** It is bounded by the 1 MiB ceiling and by *K*.
- **Two implementations of resolution**, Rust and TypeScript, held equal by shared vectors rather
  than by one shared binary. CEL in the till (ADR-0193) may later make one of them unnecessary.
- **A store on an old release stays on an old version.** This is deliberate: the alternative is a
  till that draws components it does not know. OTA rings bring it forward.
- **Choosing from the till itself** is not offered: a choice is a write to cloud configuration, and
  the edge does not write configuration (roadmap-v3 D18).
