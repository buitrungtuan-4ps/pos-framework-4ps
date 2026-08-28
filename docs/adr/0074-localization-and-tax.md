# ADR-0074 — Localization & tax: authoring tax rates the edge already knows how to apply, and surfacing countries, locale packs, and store timezone as master data

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-28
**Relates to** [ADR-0027](0027-country-modules.md) (the `CountryModule`/`CountryRegistry`, `LocalePack`, and `TaxRateTable` types this surfaces and authors) · [ADR-0033](0033-config-tree.md) (the config tree the `tax` and `locale` nodes publish onto) · [ADR-0066](0066-cloud-catalog.md) (the `TaxClass` bucket and the menu compiler this extends) · [ADR-0014](0014-datetime-library.md) (the store timezone + cutoff this makes authorable) · [ADR-0043](0043-translation-grid.md) (the grid this gives dynamic locales) · `docs/cloud-admin-ux-plan.md` (Track M4)

**Context.** The runtime already computes tax correctly and applies it on the edge — the gap is that
nothing lets an operator *author* the inputs, and the cloud surfaces none of the localization model
that the domain types already carry.

1. **Tax is built but starved of values.** `pos_proto::locale` has `TaxRate`, `TaxRateRow`, and a
   `TaxRateTable` keyed by (tax class × sales channel); `pos_core::billing::assemble` and
   `pos_core::menu::reprice_line` apply the channel-keyed rate and refuse a missing one
   (`TaxRateNotConfigured` / `MissingRate`); the edge holds `EdgeSession.tax_rates` and bills against
   it. But that table is populated **only** by `EdgeSession::bootstrap` — a single hardcoded row
   (`Standard`, dine-in, 10%). There is no `tax_rates` storage, no editor, no publish route, and
   `session_from_config` reads no `tax` node, so an authored rate has nowhere to land. The plan calls
   this out: *"TaxClass is a name-only label; the per-(class × channel) rate table has no editor and is
   never read by the edge session."*
2. **Countries and locale packs are types with no surface.** `pos-country` defines `CountryModule`,
   `CountryRegistry`, and a `country_registry!` macro, and `countries/zz` is a full reference module
   (currency, number format, default language, a per-channel tax table, fiscalization). But `pos-cloud`
   does not even depend on `pos-country`; it builds no registry and exposes no countries, currencies,
   or locale packs. So there is no currency picker and no source of truth for "what locales exist".
3. **Store timezone and business date are hardcoded.** `StoreTimeZone`/`CutoffHour` and
   `derive_business_date` exist (ADR-0014), but `EdgeSession` sets them only in `bootstrap()` to UTC /
   04:00; they are in neither the registry nor the config tree, so every store computes its business
   date in UTC.
4. **The translation grid is locale-hardcoded in the UI.** The persisted grid
   (`Record<key, Record<locale, message>>`) already carries arbitrary locales, but the Translations
   screen iterates the compiled-in `LOCALES = ["en","vi"]` for its columns, so a `ja`/`ko` value is
   invisible, and there is no completion signal or missing-only view.
5. **Menu names are single-dimension.** `CatalogItem.name` is one string; the compiler emits one
   `MenuEntry.display_name`; the `MenuBook` is keyed by channel only. There is no per-locale name.

**Decision.** Author the localization inputs in the cloud and publish them onto the config tree the
edge already consumes, reusing the domain types wholesale — this track adds *authoring, storage,
publish, and read surfaces*, not new tax or locale mechanics.

1. **Tax rates (the headline).** A new tenant-scoped `catalog_tax_rates` table holds one row per
   (tenant, tax class, sales channel) with the rate in **basis points** (matching `TaxRate`, which is
   integer by `clippy.toml`'s no-float rule and to keep a rate off a legal document as `0.09999`). A
   `TaxRateStore` seam (`list` / `set` the whole tenant table) with a `store-postgres` adapter and a
   fake. Admin routes `GET`/`PUT /admin/catalog/tax-rates` behind `ManageCatalog` (the permission tax
   *classes* already use), audited (`tax_rate.set`), validating that each row names a known class and a
   known channel and a rate within `[0, 10000]` bps. Publish assembles the rows into a `TaxRateTable`,
   serializes it as the **`tax` config node** onto the Store layer (config index 2), and versions it via
   `ConfigTree::publish` — exactly the merge-a-data-node shape the capabilities node uses. The edge's
   `session_from_config` gains a `tax` branch that parses the node into `session.tax_rates`; the
   **never-blank rule** keeps the bootstrap default if the node is absent or unparseable, so a store
   already running does not lose its rates to a bad publish. `rate_for` still returns `None` (not zero)
   for a class nobody priced — a missing rate stays a visible configuration error, not a silent
   zero-tax sale.
2. **Country registry as read-only master data.** `pos-cloud` depends on `pos-country` and builds a
   `CountryRegistry` from the compiled-in modules at boot. `GET /admin/countries` lists each module
   (country code, display name, currency, default language, and its default tax rows) and
   `GET /admin/locales` lists the locales the compiled modules declare — both behind `Read`. These feed
   the currency picker (§3), the tax-rate editor's channel/class defaults, and the translation grid's
   column set (§4). **Production country modules with real fiscalization (VN e-invoice, JP qualified
   invoice) are a flagged follow-up** — this track ships the registry infrastructure and the reference
   `zz` module it already carries; a hollow-but-honest surface beats none, and adding a module later is
   purely additive.
3. **Store locale & timezone settings.** A store-level settings record (country code, currency code,
   IANA timezone, business-date cutoff hour) stored tenant-scoped and edited behind `ManageStores` (a
   store property, not catalog). Publish writes a **`locale` config node** onto the Store layer; the
   edge applies its timezone and cutoff into `session.timezone` / `session.cutoff` (and carries the
   currency for display), killing the hardcoded UTC / 04:00. Business-date *display* then just calls the
   existing `derive_business_date`. The node also carries an optional `display_language` — the locale
   the store renders item names in (decision 5); absent, each item shows its default name.
4. **Translation grid: dynamic locales, completion %, missing-only.** Dashboard-only. The grid's column
   set becomes the union of the locales the grid already carries and the locale catalogue from
   `GET /admin/locales`, replacing the hardcoded `["en","vi"]`; a per-locale completion percentage
   (non-empty cells ÷ keys) and a missing-only filter are computed client-side. No schema or wire
   change — the persisted grid is already locale-agnostic, and `en` stays the enforced fallback.
5. **Per-locale item names — additive.** `CatalogItem` gains an optional `name_translations`
   (locale → name), stored as a jsonb column on `catalog_items` (migration 0029, `NOT NULL DEFAULT
   '{}'`); the menu compiler emits an **additive** `MenuEntry` `display_name_translations` map alongside
   the existing `display_name`, which stays the fallback. Selection happens **once, at the edge, at
   config install**: the store's `display_language` — an optional field added to the `locale` node
   (decision 3) — picks each entry's name via `MenuCatalog::localized`, folding it into `display_name`
   so the priced line, receipt, and KDS all read in the store's language with no change to the reprice
   contract; an item with no translation for that language keeps its default name (never-blank), and a
   store that sets no language shows every default name exactly as today. This is a **wire-additive**
   change (a new optional field, nothing renamed or removed), so it does **not** bump
   `PROTOCOL_VERSION`. Per-locale names start with items; category / layout / menu-section labels, and a
   per-device (rather than per-store) render language, are a later, mechanical extension of the same
   shape.

**Permissions.** No new `ConsolePermission`. Tax rates reuse `ManageCatalog` (Owner/Admin — tax classes
already there); store locale settings reuse `ManageStores` (Owner/Admin); the country/locale reads and
the grid completion view reuse `Read`; publish reuses `PublishConfig`; the grid stays on
`ManageTranslations`. Deny-by-default and the role table are unchanged.

**Consequences.**
- The single most load-bearing M4 gap closes first: an authored (class × channel) rate reaches the edge
  and is applied to real bills, with a missing rate still a loud error.
- Nothing here is a breaking wire or protocol change: the `tax` and `locale` config nodes are additive
  Store-layer keys (a store without them behaves exactly as today), the per-locale name field is an
  additive optional, and no migration renames or drops.
- The config tree gains two more Store-layer keys; the merge-on-publish path already preserves sibling
  keys (`menu`, `layout`, `permissions`, capability flags), so publishing tax does not clobber the menu.
- The edge's never-blank apply rule means a malformed `tax`/`locale` node degrades to the last good
  value rather than to blank rates or a UTC clock.

**Alternatives considered.**
- *Rates in the registry `StoreRecord` or the locale pack only.* Rejected: the (class × channel) table
  is tenant-scoped catalog data an operator edits per class, not a per-store identity field, and the
  edge already reads rates from `session.tax_rates`, which the config tree — not the registry — feeds.
- *A locale dimension on `MenuBook` (locale × channel).* Rejected for now as a larger wire change with a
  combinatorial compiled size; an additive per-entry name map delivers per-locale item names without
  re-shaping the book, and can be superseded later if a fully localized book is warranted.
- *Names as translation keys resolved on-device against the grid.* Rejected as the default: it couples
  every item name to the translation runtime and loses the authored name as the diffable source; the
  additive map keeps the name with the item while still allowing per-locale values.
- *Building a full production VN/JP country module now.* Deferred: real fiscalization is a substantial,
  legally exacting effort (VN e-invoice ranges, JP qualified-invoice numbering) that the plan already
  lists as deferred telemetry/fiscal work; the registry surface does not depend on it.

**Deferred / flagged follow-ups.**
- Receipt templates + brand logo/footer (Track M4's last bullet) — depend on **M5** media/branding and
  land with it.
- Production country modules with fiscalization (VN, JP).
- Per-locale labels for categories, menu sections, and layout buttons (item names first).
