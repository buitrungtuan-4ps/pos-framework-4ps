# `pos-cloud-catalog`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

The cloud's catalog, with its fees: the item master, menus, taxonomies, courses and layout, and the compile of a menu to the `menu` and `layout` nodes a store runs; the fee rules, checked and compiled against the catalog; item images and their renditions; tax rates per class and channel; the translation grid; the item master's CSV import and export; and their routes and stores. Catalog and fees are one crate because each uses the other: the menu publish republishes fees, and a fee rule is checked against the catalog ([ADR-0175](../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md) decision 2). It stands on `pos-cloud-api` (authentication, the audit trail and the refusals), `pos-cloud-config` (the publish every area writes through, and the registry) and `pos-cloud-reports` (the revenue CSV writers its `export` module re-exports). `pos-cloud` re-exports `catalog`, `catalog_compiler`, `fees`, `media`, `images`, `tax`, `import`, `export` and `translations` under their old paths, and imports each area of `routes` into its `http` module under its old name.

## Owns

- The catalog and the `CatalogStore` trait; the menu and layout compile
- Fee rules and the `FeeRuleStore` trait
- Media and the `MediaStore` trait; image renditions
- Tax rates and the `TaxRateStore` trait
- The translation grid and the `TranslationStore` trait
- The item master's CSV import and export
- Their routes: `/admin/catalog/*` (with the tax rates and the menu publish), `/admin/fees/*`, `/admin/media/*` and `/admin/translations/*`
- Its stores over PostgreSQL, through `store-postgres`

## Depends on / used by

- **Depends on** (workspace crates): `pos-cloud-api`, `pos-cloud-config`, `pos-cloud-reports`, `pos-core`, `pos-proto`, `store-postgres`.
- **Used by** (workspace crates): `pos-cloud`, `pos-cloud-floor`.

## Entry points

- `src/catalog.rs`: `CatalogStore` and the catalog's records; `src/catalog_compiler.rs`: `compile_menu`, `compile_layout_book`
- `src/fees.rs`: `FeeRuleStore`, `FeeRule`, the resolve and compile of a store's fees
- `src/media.rs`, `src/images.rs`: `MediaStore`, renditions
- `src/tax.rs`: `TaxRateStore`; `src/translations.rs`: `TranslationStore`
- `src/import.rs`, `src/export.rs`: the item master as CSV
- `src/routes.rs` and `src/routes/`: `catalog_router`, `catalog_publish_router`, `fees_router`, `media_router`, `tax_rate_router`, `translation_router`
- `src/persistence.rs`: the store-postgres impls

## Rules

- [ADR-0043](../../docs/adr/0043-translation-grid.md): the translation grid
- [ADR-0066](../../docs/adr/0066-cloud-catalog.md): the cloud catalog
- [ADR-0074](../../docs/adr/0074-localization-and-tax.md): localization and tax
- [ADR-0075](../../docs/adr/0075-media-and-file-rail.md): media and the file rail
- [ADR-0159](../../docs/adr/0159-a-fee-is-configuration.md): a fee is configuration
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4 and [ADR-0175](../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md) decision 2: why this is a crate, with fees inside

## How to test

```bash
cargo test -p pos-cloud-catalog
```

The routes' HTTP tests drive the whole router, so they stay in `crates/pos-cloud/tests/`.
