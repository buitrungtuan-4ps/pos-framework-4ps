# ADR-0246 — A setting has one source of truth, and publish and rollback take a scope

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 (open item O3) · Amends
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md) decision 3 ·
Relates to [ADR-0033](0033-config-tree.md),
[ADR-0122](0122-a-store-group-is-a-delivery-cohort.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md)

## Context

A rollback restores a store's published config tree. The settings HQ authors live in a store of
their own and are compiled into the tree on every publish, so the next publish, even an unrelated
one, recompiles the rolled-back values and silently undoes the rollback (phase 10, F10). History
shows only times, not who changed what or where.

## Options considered

1. **The tree is the source**; authoring writes the tree. Loses the per-scope authoring ADR-0160 built.
2. **The authored values are the source; a rollback writes them back**, and the tree is always
   compiled from them.

## Decision

**Option 2.**

1. **One source per setting.** The authored value at its scope is the only source. The published
   tree is always compiled from the authored values, never edited on its own.
2. **A rollback is a write.** Rolling back restores the authored values of a chosen earlier revision,
   records that as a new revision, and publishes. A later publish compiles the restored values, so it
   can no longer undo a rollback.
3. **Publish and rollback take a scope:**
   - *what*: one setting, one group (a node: `einvoice`, `billing`, `vendors`, the menu …) or the
     whole package;
   - *where*: one store, a store group, or the tenant.
   A rollback touches only that scope: other settings, other groups and other stores keep their
   values.
4. **History says who, what and where**: each revision records the person, the scope, and the
   difference in words ("`billing.card_confirm_approval`: A manager → Nobody else, at store Q1").
5. **Fixed:** a rollback cannot restore a value the register refuses now (a country limit, a forbidden
   value); it is refused, naming the setting.

## Consequences accepted

- A migration that records revisions of authored values, additive. Existing tree history stays
  readable but cannot be scoped.
- More rows per publish; bounded by the register's size.
