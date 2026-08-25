# ADR-0065 — The cloud org registry: named Tenant/Brand/Store/Device, the hierarchy config never recorded

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-25
**Relates to** [ADR-0004](0004-cloud-owned-configuration.md) · [ADR-0033](0033-config-tree.md) · [ADR-0041](0041-device-onboarding.md) · [ADR-0051](0051-device-credential-provisioning.md) · [ADR-0060](0060-cloud-back-office-dashboard.md) · [ADR-0034](0034-super-admin-auth.md) · [ADR-0037](0037-api-keys.md) · `docs/roadmap.md` WS-C

**Context.** The cloud has always addressed a store by two opaque ULIDs, `(tenant_id, store_id)`. Nothing
records that a tenant, brand, or store *exists*, what it is *called*, or which brand and tenant a store
*belongs to*. `config_trees` is keyed `(tenant_id, store_id)` and holds four deep-merge layers named
Tenant → Brand → Store → Device ([ADR-0033](0033-config-tree.md)), but "Brand" there is only a *layer
name inside one store's blob*: there is no Brand entity and no store→brand parentage. ADR-0033 says so
outright — "a shared Tenant/Brand layer that fans out to every store under it is a future modeling
step; today each store's tree holds its own four layers."

The product pays for this. The back-office dashboard ([ADR-0060](0060-cloud-back-office-dashboard.md))
can only offer **free-text ULID entry** for the tenant/store context, and a mistyped value surfaces the
raw `tenant_id is not a ULID`. A non-technical operator cannot use it, and there is **no "create a
store" flow** at all — onboarding is ULID-by-hand. Device identity, meanwhile, is spread across
purpose-specific tables — `device_proposals` (the discover→approve queue, [ADR-0041](0041-device-onboarding.md))
and `device_credentials` (issued secrets, [ADR-0051](0051-device-credential-provisioning.md)) — with no
canonical "this device exists, is named X, is a printer, belongs to store Y" record.

**Decision.**

- **A registry of four named entities** — `tenants`, `brands`, `stores`, `devices` — in `store-postgres`
  (migration `0011`). Each is a ULID-keyed row carrying a human `name`, a `status` (`active` / `archived`,
  never hard-deleted so history and foreign references stay valid), `created_at` / `updated_at`, and its
  parentage: a brand names its tenant; a store names its tenant and (nullable) brand; a device names its
  tenant and store. This is the hierarchy that `(tenant_id, store_id)` always implied but never wrote down.

- **The registry owns identity and naming; the config tree keeps owning configuration.** `config_trees`
  is untouched — still keyed `(tenant_id, store_id)`, still the sole source of truth for a store's four
  merge layers and their version history. The registry answers *what exists, what it is called, what it is
  under*; the config tree answers *how it is configured*. A `stores` row and its `config_trees` row share
  the same `(tenant_id, store_id)`. There is no data migration of `config_trees` and no change to config
  delivery ([ADR-0039](0039-config-delivery.md)).

- **RLS by tenant, exactly as `config_trees` and the rollups.** `brands`, `stores`, and `devices` each
  carry `tenant_id`, `ENABLE ROW LEVEL SECURITY`, a `USING (tenant_id = current_setting('app.tenant_id',
  true))` policy, and a `GRANT` to `app_tenant`; the trusted pool owner (the super-admin path) bypasses
  RLS to administer any tenant, matching every other cloud table. `tenants` is the root and has no parent
  to scope by, so — like `super_admin` ([ADR-0034](0034-super-admin-auth.md)) — it is administered only by
  the trusted connection and carries no `app_tenant` grant. All tables are `CREATE TABLE IF NOT EXISTS`,
  forward-only and additive, applied idempotently on boot ([ADR-0017](0017-migrations.md)).

- **`devices` is the canonical device identity; the existing tables key to it, not duplicate it.** A
  `devices` row is the one record that a given device exists, is named, has a kind (POS / printer / KDS /
  tablet …), and belongs to a store. `device_proposals` stays the inbound discover→approve queue and
  `device_credentials` stays the issued-secret store; both continue to key by the same `device_id`, and an
  approval / provisioning writes the canonical `devices` row. One identity, three concerns — not three
  identities.

- **A `RegistryStore` seam local to `pos-cloud`**, alongside `ConfigTreeStore` and `RollupStore` (the
  cloud stores are cloud-local traits, not the domain's seventeen ports). It offers list / get / create /
  rename / archive for each entity; the `store-postgres` implementation scopes by tenant through the
  `app.tenant_id` setting and the trusted connection, and an in-memory fake passes the same behaviour in
  tests, as every other cloud store does.

- **A one-time, idempotent backfill seeds the registry from `config_trees`.** On first run after `0011`,
  every distinct `(tenant_id, store_id)` already present in `config_trees` becomes a `stores` row (and its
  `tenant_id` a `tenants` row) with a **placeholder name** (`Store <short-ulid>`), status `active`, and no
  brand — so no existing store is orphaned, the dashboard shows the whole cell immediately, and *naming*
  them is a non-blocking follow-up. The backfill is insert-if-absent (safe to re-run) and never touches
  `config_trees`.

- **Admin routes follow the [ADR-0060](0060-cloud-back-office-dashboard.md) pattern, under the existing
  super-admin session.** List/create/rename/archive for each entity, with the tenant named by a
  `?tenant_id=` query for the sub-tenant entities — the same admin-is-global read the config and rollup
  routes already use. No new auth surface, no cookie change.

**Rejected.**

- **Reshaping `config_trees` to carry parentage, or introducing the shared Tenant/Brand layer now** —
  rejected here. That shared-layer fan-out is the modeling step ADR-0033 defers; it changes the merge and
  pull semantics and reaches the edge. The registry records parentage *without* touching config delivery,
  and the shared-layer / master-push work is a separate, later decision that will *build on* this
  parentage (it needs to know which stores sit under a brand — which only the registry records).

- **A `(tenant_id, brand_id, store_id)` config key** — rejected: it forks the `(tenant_id, store_id)` key
  every table and the edge already use. Parentage as a nullable registry column is additive and leaves the
  established key alone.

- **A `devices` identity separate from `device_proposals` / `device_credentials`** — rejected: three rows
  claiming to be "the device" is exactly the duplication this ADR removes. The registry row is canonical;
  the other tables reference its `device_id`.

- **Putting the registry in `pos-ports`** — rejected: it is cloud back-office machinery, not domain, so it
  follows the cloud-local store traits, not the seventeen ports.

- **Storing names in the event log** — rejected: a tenant/brand/store name is mutable metadata, not an
  immutable business event; it lives in a mutable registry table (like `config_trees`), never in the
  append-only log.

**Consequences.**

- The dashboard can replace free-text ULID entry with **named pickers** and offer a **create-store flow**
  (the WS-C onboarding wizard, follow-up PRs). `tenant_id is not a ULID` stops being reachable because the
  operator selects from a list rather than typing an identifier.
- **Parentage now exists**, which is the prerequisite for the deferred master-push / shared-layer work:
  "publish at Brand → every store under it" needs to know which stores are under a brand, and only the
  registry records that.
- **Device identity is unified**: one named row per device, with the proposal queue and the credential
  store keying to it.
- **Data classification.** Tenant / brand / store names, addresses, and timezones are internal business
  metadata (T3): mutable, non-sensitive, and never customer or employee PII. The registry holds no T1
  data, so nothing here changes the retention or masking rules ([ADR-0035](0035-retention-and-pii-masking.md)).
- **Backfill** means upgrading an existing cell surfaces its stores at once under placeholder names, with
  no manual step and no risk of losing a store that has config but no registry row.
- **Deliberately not here yet:** the create-store *wizard* and the named pickers (dashboard, follow-up
  PRs); brand/tenant *config fan-out* / master-push (Phase 2, its own ADR); and multi-user RBAC with the
  scope-check middleware that will replace the `?tenant_id=` query trust once the super-admin-is-global
  assumption of [ADR-0034](0034-super-admin-auth.md) is superseded (Phase 2b, its own ADR).
