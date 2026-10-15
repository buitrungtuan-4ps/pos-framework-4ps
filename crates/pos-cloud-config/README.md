# `pos-cloud-config`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

The cloud's configuration base: what every business area's routes publish through. It holds the four-level configuration tree and the store it persists through, the publish that writes an area's nodes onto a store's layer under a conditional write with the prerequisite and settings checks every publish runs, and what the configuration routes share. It also holds what that publish reads: the org registry, the settings tenants write once for many stores, and QR ordering's one switch. It holds no routes. `pos-cloud` re-exports `config_tree`, `registry`, `settings`, `confirmed_values`, `qr_ordering` and `data_migrations` under their old paths, and keeps the routes that compose every area's nodes (store groups, releases, the config history).

## Owns

- The configuration tree: its layers, merge, validation, versions and history, and the `ConfigTreeStore` trait
- The publish every area's routes write through: `publish_config_nodes`, its conditional-write retry, the prerequisite check, and the recorded-confirmation check on a settings value
- What the configuration routes share: the tenant query, the published version, the refusals for an unpublished store or an unknown level or status, a node read as the store runs it
- The org registry: tenants, brands, stores and devices, and the `RegistryStore` trait
- Settings: the values a tenant writes once for many stores, and the `SettingsStore` trait
- QR ordering's one switch, which every Store-layer publish aligns, and the record of the one-time data change that settled it
- Its stores over PostgreSQL: the config-tree, registry, settings and data-migration tables, through `store-postgres`

## Depends on / used by

- **Depends on** (workspace crates): `pos-cloud-api`, `pos-core`, `pos-ports`, `pos-proto`, `store-postgres`; in tests, `pos-fakes`.
- **Used by** (workspace crates): `pos-cloud`, `pos-cloud-catalog`, `pos-cloud-devices`, `pos-cloud-floor`, `pos-cloud-inventory`.

## Entry points

- `src/config_tree/`: `ConfigTree`, `ConfigTreeState`, `ConfigLevel`, `ConfigTreeStore`
- `src/publish.rs`: `publish_config_nodes`, `publish_config_nodes_with`, `load_tree_for_write`
- `src/route.rs`: `RegistryTenantQuery`, `PublishedConfig`, `read_store_node_as_run`, `config_store_error_response`
- `src/setting_refusal.rs`, `src/confirmed_values.rs`: the settings refusals and the recorded-confirmation check
- `src/registry.rs`: `RegistryStore`, `EntityStatus`
- `src/settings.rs`: `SettingsStore`, `resolve_for_store`
- `src/qr_ordering.rs`, `src/data_migrations.rs`: QR ordering's switch and its migration
- `src/persistence.rs`: the store-postgres impls

## Rules

- [ADR-0033](../../docs/adr/0033-config-tree.md): the configuration tree
- [ADR-0065](../../docs/adr/0065-cloud-org-registry.md): the org registry
- [ADR-0095](../../docs/adr/0095-conditional-writes-for-collections.md): conditional writes
- [ADR-0160](../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md): settings are published configuration
- [ADR-0169](../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md): a setting value can require a recorded confirmation
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4: why this is a crate

## How to test

```bash
cargo test -p pos-cloud-config
```
