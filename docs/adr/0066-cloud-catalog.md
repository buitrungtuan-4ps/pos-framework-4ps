# ADR-0066 — The cloud catalog: one normalized authoring model that compiles to per-channel flat menu snapshots

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-25
**Relates to** [ADR-0004](0004-cloud-owned-configuration.md) · [ADR-0033](0033-config-tree.md) · [ADR-0039](0039-config-delivery.md) · [ADR-0063](0063-store-menu-catalog.md) · [ADR-0065](0065-cloud-org-registry.md) · `docs/pos-spec.md` §5, §23.1 · `docs/roadmap.md` Phase 2a
**Amends** [ADR-0063](0063-store-menu-catalog.md): the config `menu` node becomes a channel-keyed `MenuBook`, of which a single `MenuCatalog` is the degenerate case.
**Unblocks** the back-office menu editor, channel-specific pricing, per-channel layouts, and master-menu push to many stores at once.

## Context

[ADR-0063](0063-store-menu-catalog.md) gave the store an authoritative **flat** menu: a `MenuCatalog` of `MenuEntry` rows (`menu_item_id`, `display_name`, `unit_price`, `tax_class_id`, `available`), delivered on the `menu` node of the config tree, from which `pos-core::reprice_line` prices every inbound line. Tax composes with a channel-keyed `TaxRateTable` (D6): `tax_class` on the item, the rate resolved from `(tax_class, sales_channel)` at reprice time. That contract is fixed, tested, and forward-compatible, and it is deliberately **thin** — a price book that computes nothing.

ADR-0063 also said, in as many words, that **the back office that populates the menu node is separate and not yet built**. That is this ADR. It is a core-framework requirement, not a Pizza 4P's convenience: an international operator needs

- **many menu sets that inherit** — a tenant standard, a brand override, a store special, a Grab-only menu, a happy-hour menu — resolved most-specific-wins;
- **prices that differ per channel** — dine-in, takeaway and delivery are genuinely different amounts for the same item — and **taxes that differ per channel and per country** (the worked case: 8% takeaway, 10% dine-in for one item);
- **an item-master taxonomy** (category / sub-category for reporting, tax defaults and kitchen grouping) that is **distinct from a display taxonomy** (the categories and sub-categories a screen groups buttons under);
- **per-channel layouts** — a POS terminal lays out buttons by display category with an x/y grid; a tablet, a QR guest page and a Grab feed each present the same items differently;
- **multi-currency and multi-country**, plug-and-play by tenant / brand / store / channel;
- and the reason all of this exists in the cloud at all: **one edit pushed down to a whole fleet of stores**.

Today none of this has a home. There is no authoring model, no category/section/display/layout concept anywhere in `pos-proto`/`pos-core`/`pos-cloud`, the compiled catalog carries a single channel-agnostic price, and the config `menu` node is not yet parsed at the edge at all. This ADR fixes the model before the code that governs it (ADR-before-code).

## Decision

- **Two tiers, one compiler between them.** The cloud holds a **rich, normalized catalog** — the source of truth an operator edits. The edge receives only a **thin, flat snapshot** — exactly the `MenuCatalog` shape it already reprices from. A pure, deterministic **compiler** turns one into the other at publish time. The edge never sees a menu, a section, a price list, an inheritance rule or a layout grid; it sees a resolved price book and a resolved display plan. This keeps the store's "computes nothing" promise (ADR-0063) intact while all richness lives where it can be queried, related and validated.

- **Twelve authoring entities**, all tenant-scoped. Grouped:
  - *Item master & operational taxonomy* — (1) **Item**: the product master (i18n names, `tax_class`, SKU, recipe/BOM reference, item-category); (2) **Item Category** and (3) **Item Sub-category**: the operational taxonomy for reporting, tax defaults and kitchen grouping — **not** what a screen shows.
  - *Modifiers* — (4) **Modifier**: itself an item priced in the same money (ADR-0063 — a modifier is a `menu_item_id`); (5) **Modifier Group**: a set with min/max selection rules, attached to items.
  - *Menus* — (6) **Menu**: a named set that may **inherit** from a parent menu; (7) **Menu Section**: an authoring grouping within a menu; (8) **Menu Placement**: an item placed in a section, with an overridable price and availability — the row that compiles to a `MenuEntry`; (9) **Price List**: channel-keyed (and currency-keyed) prices a menu draws from.
  - *Tax & money* — (10) **Tax Class + Rate Table**: `tax_class` on the item (country-agnostic), rates keyed by `(tax_class, channel)` per country — the existing `TaxClassId` and `TaxRateTable` reused unchanged.
  - *Presentation* — (11) **Display Category / Display Sub-category**: the presentation taxonomy a layout groups by, independent of item categories; (12) **Layout**: a per-channel presentation plan — which display categories show, item order, and the POS button grid (x/y) per category.

- **Channel is resolved at compile time; the compiled `menu` node becomes a `MenuBook`.** Because prices differ per channel but `MenuEntry` carries one `unit_price`, the compiler produces **one flat `MenuCatalog` per channel** and packs them into a new `MenuBook` (channel → `MenuCatalog`) — a `pos-proto` shape added additively, mirroring `TaxRateTable`'s channel-keyed rows. `MenuEntry`, `MenuCatalog` and `reprice_line` do **not change**; the edge session holds a `MenuBook` and selects `book.catalog_for(order.sales_channel)` before calling the unchanged `reprice_line`. A `MenuBook` may carry a default catalog for channels without a specific price list. This amends ADR-0063's "the `menu` node is a `MenuCatalog`" to "the `menu` node is a `MenuBook`" — safe, because nothing parses that node yet.

- **Layouts are a separate artifact the domain never reads.** The compiler resolves each channel's layout to a **`DisplayPlan`** (display categories/sub-categories, item order, button grid) delivered on a `layout` config node, consumed by the POS/tablet/QR/marketplace **UI** — never by `pos-core`. Presentation and pricing travel side by side but are not entangled: a layout change reprices nothing, a price change relays no buttons.

- **Assignment and resolution mirror the config tree.** A Menu and a Layout are *assigned* to a scope: `(brand | store) × channel × day-part`. For a given `(store, channel, time)` the resolver picks the most-specific applicable menu (store over brand over tenant default), folds menu **inheritance** (parent placements overridden by child), applies the channel's price list, resolves modifier groups, and turns each `tax_class` into a rate for the store's country and channel. Most-specific-wins is the same precedence [ADR-0033](0033-config-tree.md) already uses for config layers, extended by channel and day-part. The resolver is pure and property-tested.

- **Authoring lives in its own store, distinct from the config tree and the registry.** A new tenant-scoped, RLS-isolated **catalog store** (Postgres) holds the twelve entities as normalized, relatable, queryable rows — the way [ADR-0065](0065-cloud-org-registry.md) made identity/naming a store distinct from configuration. The config tree carries only *compiled output*, never the editable source; you cannot query "every item in the alcohol tax class across the tenant" against an opaque jsonb blob.

- **Publish compiles, then rides the config tree — no new channel.** "Publish" resolves the catalog for every `(store × channel)` a store sells on, writes the `MenuBook` to each store's `menu` node, the `DisplayPlan`s to its `layout` node, and the resolved `TaxRateTable` to `store.tax.tax_class_rates`, bumps the config version, and lets each store **pull** the change over the existing `/sync` path ([ADR-0033](0033-config-tree.md), [ADR-0039](0039-config-delivery.md)). No new endpoint, no new port, no push. **This is master-push**: one publish fans out to a whole fleet, delta-encoded and last-known-good-protected like any other config. Publishing is immediate — no maker-checker.

- **Tax is country-agnostic on the item, resolved per store at compile time.** `tax_class` stays on the item everywhere. The compiler, knowing a store's country and the channels it sells, emits the `(tax_class, channel) → rate` rows into that store's `TaxRateTable`. Multi-currency is one price list per currency; a store compiles only the price list in its own currency, because `Money` and `reprice_line` are single-currency by construction.

- **Availability composes, it does not fight.** The compiled `MenuEntry.available` is the operator's *published floor* — "we've paused this item" / "this menu doesn't carry it". The edge's §8 stock computation (`available = floor(min(stock/recipe))`, auto-86) can only push availability **down** at runtime; it never re-enables what the operator paused. The catalog carries each item's recipe/BOM reference so the edge can compute §8 locally. The two representations — published flag and computed availability — meet here, at the compile boundary, by this rule.

## Rejected

- **A `channel → price` map inside `MenuEntry`.** It would reshape a tested, forward-compatible contract that `reprice_line`, the `sales.order_line.added` event, and round-trip tests all pin. Compile-time-per-channel `MenuBook` is purely additive and leaves the hot path untouched — the same reason ADR-0063 put the catalog in `pos-proto` rather than reshaping the domain.
- **Shipping the rich authoring model to the edge.** It breaks the "store computes nothing" invariant, explodes the sync payload, and would make every store re-run inheritance and channel resolution offline. The compiler runs once, in the cloud.
- **A dedicated menu delivery endpoint or a `MenuStore` port.** The config tree already delivers cloud-owned configuration with versioning, deltas and last-known-good; a menu is configuration. ADR-0063 rejected the same thing for the same reason.
- **Storing the authoring model in the config-tree jsonb.** Configuration is compiled, opaque, per-store output; a catalog is a normalized, cross-store, relational source you query and validate. Conflating them loses both.
- **One taxonomy for items and display.** The operator explicitly needs an operational grouping (reporting, tax, kitchen) *and* a presentation grouping (screen tabs), and they do not coincide — a "Beverage" item may sit under a "Summer specials" display category on one channel only.
- **Maker-checker on publish.** The operator chose immediate publish; the config tree's validate-and-keep-last-good already prevents a bad version from reaching a store.

## Consequences

- **New `pos-proto` shapes, additive:** `MenuBook` (channel → `MenuCatalog`) and `DisplayPlan` (+ display taxonomy and button-grid types). `MenuEntry`/`MenuCatalog`/`TaxRateTable`/`SalesChannel` are unchanged. ADR-0063's config `menu` node is now a `MenuBook`.
- **A `CatalogStore` seam + a Postgres schema (RLS by tenant) + a fake** land in a later slice — an additive migration, in the shape [ADR-0065](0065-cloud-org-registry.md) set for the registry.
- **The compiler is the heart of the phase** — a pure `catalog + (store, channel, time) → MenuBook + DisplayPlan + TaxRateTable` function, property-tested against inheritance, channel and precedence laws, exactly as the domain suites are.
- **Edge consumption is WS-B.** Parsing the config `menu`/`layout` nodes and `store.tax.tax_class_rates` into the session (and selecting the per-channel catalog in reprice) lands in the store-side config-pull work; until then reprice runs on test-seeded catalogs, as today.
- **Build order after this ADR:** (1) proto shapes `MenuBook`/`DisplayPlan`; (2) `CatalogStore` seam + Postgres adapter + fake; (3) the resolver/compiler in a pure crate; (4) cloud catalog CRUD admin routes; (5) publish (compile → write config tree); (6) the dashboard menu editor; (7) edge parsing (WS-B). Each is its own reviewable PR.
- **Data classification.** Catalog prices and tax structures are **T2 (pricing models)** — authored and compiled in the cloud, never written to logs or events; the compiled snapshot carries prices to the store as configuration, which is already how `MenuCatalog` travels. Item and display names are T3.
- **Reporting is unblocked later:** an item category is what a product-mix report (D10) groups by.
