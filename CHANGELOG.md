# Changelog

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows [Semantic Versioning](https://semver.org/) for the product and a separate `PROTOCOL_VERSION` for the cloud–edge wire format (see [`docs/naming-and-api.md`](docs/naming-and-api.md) §11).

**Rules for entries**

1. Every user-visible change gets an entry, written for the reader, not the author.
2. Categories: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`.
3. Reference the issue or pull request number.
4. Add an **Upgrade note** whenever the change affects `PROTOCOL_VERSION`, a migration, a permission identifier, or a default value.
5. Nothing is ever removed without having been deprecated for at least two releases.

---

## [Unreleased]

### Fixed
- **The admin console no longer surfaces `tenant_id … is not a ULID`, and every scoped screen loads
  its data on open** (Track F, F0). The `/admin` screens guarded the working context inconsistently —
  several fired their first request with an empty tenant/store id and surfaced the raw backend
  `… is not a ULID` `400`, and every screen sat blank behind a manual "Load" button. A shared context
  contract (`dashboard/src/lib/scoped.tsx`) replaces both: `RequireContext` renders a screen — and
  lets it fetch — only once its tenant, or tenant *and* store, is chosen, showing a "pick it in the
  top bar" panel otherwise; `onScopedContext` loads on mount and again whenever the context changes,
  but never with an empty id. Every scoped screen (Reports, Stores, Catalog, Layout, Config, API keys,
  Devices, Webhooks, Translations, Activation) now uses it, and the manual Load button became a
  Refresh. A `401` from any call now drops the client's authed flag so the shell returns the operator
  to the login screen instead of stranding them on a view that can no longer load, and the guided
  new-store wizard mints its store exactly once even if the operator steps back and forward.
  **Upgrade note:** none — operator-UI behaviour only; no schema, protocol, or permission change.
- **The "success" green now clears WCAG-AA contrast as text in the light theme** (P6 exit criterion,
  #44). A numeric audit of the design-token palettes (`docs/wcag-contrast-audit.md`) measured every
  colour pair the interface renders, in both themes, and found the light-theme `--ok` token at 3.72:1
  on `surface` — below the 4.5:1 needed for normal text, and `ok` is used as small text (the fired-line
  badge, the shift-variance line, the paired/settled confirmations). It was darkened from
  `oklch(0.6 …)` to `oklch(0.52 …)`, giving 5.16:1. Every other text pair already passed. A new
  `pnpm contrast` gate parses `tokens.css` and fails the build if any text pair drops below AA; it runs
  in both the `ui` and `dashboard` builds and CI. The remaining sub-3:1 tokens (the 1px separator and
  the table-state dots) are non-text and exempt — the dots are `aria-hidden` and always ride with a
  text label, so meaning never depends on the hue. **Upgrade note:** a default value moved — the light
  `--ok` token is darker; the dark theme is unchanged.
- **Super-admin sign-in after enrolment now works with any authenticator app** (ADR-0034 amended).
  The mandatory TOTP second factor ran over HMAC-**SHA256**, but Google Authenticator and Microsoft
  Authenticator ignore the `otpauth://` URI's `algorithm` field and always compute **SHA1** — so the
  6-digit codes never matched and `/admin/login` rejected every attempt with the generic "code or
  password not accepted" right after `/admin/setup`. TOTP now runs over **HMAC-SHA1** (the RFC 6238
  default that every app supports), and the provisioning URI states `algorithm=SHA1`. The 20-byte RFC
  6238 SHA1 test vectors and the URI assertion prove it. **Upgrade note:** only the server's TOTP
  HMAC changes (SHA256 → SHA1); the stored secret is untouched. An operator who used Google or
  Microsoft Authenticator — the apps whose SHA1-only behaviour caused the failure — can simply sign
  in once this deploys, because their existing entry already computes SHA1 and now matches. An
  operator whose app *did* honour SHA256 (Aegis, 1Password, FreeOTP) must re-enrol via
  `deploy/reset-admin.sh` (the `reset_admin` break-glass, ADR-0045), since the one-time secret cannot
  be re-exported. There is at most one super-admin.

### Added
- **The operator console gains a reusable CRUD kit, and the master-data screens are rebuilt on it**
  (roadmap v2, Track F2). A new `dashboard/src/components/kit.tsx` adds `DataTable` (sortable columns,
  empty-state slot, row actions), `Modal`/`Drawer`, a `ConfirmDialog` with optional type-the-name
  gating for destructive actions, `StatusBadge`, `EmptyState`, `FormField`, `TechnicalDetails` (a
  disclosure that keeps an entity's ULID present but out of the way), and a `ReorderList` primitive.
  The five simplest screens now build on it — API keys, Webhooks, Devices, Stores, and (lightly)
  Translations — so every list is a consistent sortable table, every destructive action (revoke,
  delete, reject, archive) goes through a confirmation instead of a one-click button, ULIDs move
  behind a "Technical details" disclosure, and write outcomes surface as toasts. Stores keeps inline
  brand reassignment, and the `DataTable` gained built-in **search + pagination** (client-side, over
  the rows a screen already holds — right-sized for the admin lists' volumes). **Still to come in F2**
  (flagged): AIP-193 errors on `/admin`, read-one endpoints, and `ETag`/`If-Match` concurrency;
  server-side list push-down (with `(tenant_id, created_at)` indexes and `/admin` in the OpenAPI
  drift-gate) is deferred until a list is large enough to need it, rather than churning every store
  seam and the `/admin` response shape now.
- **The admin console gains a framework-standard shell: grouped scope-aware nav, breadcrumbs, a
  command palette, toasts, a notification center, org-switcher search, and locale persistence**
  (roadmap v2, Track F1). The flat ten-item nav is now five labelled groups, each item carrying a dot
  that fills once its working context (tenant, or tenant and store) is set; a breadcrumb strip shows
  tenant › store › page; Cmd/Ctrl-K (and a top-bar button) opens a command palette that jumps to any
  screen by name; a shared toast primitive (`toast.ok`/`toast.error`) surfaces outcomes and keeps a
  short history behind a notification bell; the org switcher filters tenants and stores by name and
  caches the loaded lists across opens; the chosen language is remembered per browser and restored on
  load (falling back to the browser preference, then the `en` floor), the switcher shows each
  language's own name, and the document title and `<html lang>` track the active locale; and a
  version footer names the running build (`VITE_APP_VERSION`). Frontend only; routing unchanged.
  **Deferred within F1** (flagged follow-up): URL-encoded working context (`/t/:tenant/s/:store/…`),
  entity search in the palette (needs the F2 data layer), and in-app links to the shipped operator
  guides (not yet web-served).
- **The context picker can create a tenant, auto-disabled webhook endpoints can be re-enabled, and
  hashed dashboard assets are cached immutably** (Track F, F0). A fresh install's empty registry was a
  dead end — the picker read "No tenants yet." with nowhere to go — so it now carries an inline field
  that mints a tenant, selects it, and loads its (empty) store list, flowing straight on to the first
  store. A webhook endpoint the delivery task auto-disabled after a day of failures can now be
  re-enabled from the Webhooks screen (`POST /admin/webhooks/{id}/enable`, super-admin only,
  tenant-scoped like deletion); delivery resumes from the endpoint's stored cursor, so nothing that
  queued while it was down is skipped. And the embedded dashboard now sets `Cache-Control` — the
  content-hashed bundles under `assets/` as `immutable` for a year, and the `index.html` entry
  document as `no-cache`, so a new deploy is picked up on the next load rather than served stale.
- **The operator UI gains a shared component kit, and the design tokens are drift-guarded** (UX
  polish, WS-E / #104). A `PageHeader` component (the edge counterpart of the dashboard's
  `components/ui.tsx` kit) replaces the `<h1>` markup every edge screen hand-rolled; it takes a `size`
  prop so the KDS and expo screens keep their deliberately larger two-metre titles while the
  operator screens keep theirs — a faithful consolidation, no rendered change (the built CSS is
  byte-identical). The design tokens and the WCAG contrast gate are duplicated verbatim across the
  two separate front-end build roots (`ui/` and `dashboard/`), which cannot share a module; a new
  `cargo xtask mirrored-files` check — in `just preflight` and CI — fails the build the moment a
  mirrored pair diverges, so a token darkened in one root but not the other can no longer silently
  leave one theme failing AA. **Scope:** the deeper visual pass over both surfaces stays human-gated
  (it needs rendered-screen review, like the WCAG *visual* and hardware checks); this lands the
  mechanical, verifiable half.
- **A config publish that carries an unparseable menu is now rejected, not silently dropped**
  (ops hardening, WS-D / #103). The cloud's `CapabilityValidator` — the gate every publish passes,
  including the generic `PUT /admin/stores/{id}/config/{level}` route — now round-trips a `menu` or
  `layout` node through the *exact* path the edge reads them (`to_string` → `from_str`), and rejects a
  publish the store could not consume. Before, a malformed delivery node validated, published, and was
  silently ignored by the edge's forgiving `session_from_config`, leaving a store on its old menu with
  no error anywhere — a "successful" publish that never reached the counter. A `docs/security-review-ws-d.md`
  records the finding (low severity — the route is super-admin-only) and the surrounding review of the
  QR, relay, config and metrics surfaces. Forward-only; no migration, no protocol change.
- **The optional monitoring profile is now wired into `pos_cloud`** (observability, WS-D / #103,
  ADR-0031). A new `[metrics]` config section constructs the `metrics-vm` sink and spawns a sparse
  liveness heartbeat (`pos.cloud.up`) off the sales path, alongside the other background tasks. It is
  **off by default**: per `docs/capacity-and-reliability.md` the monitoring profile stays off below
  ~50 stores in favour of sparse sampling, so a pilot cell leaves `[metrics]` unset and emits nothing.
  The heartbeat carries no labels and no PII, and the sink's bounded queue drops under pressure — a
  metrics backend can never become a trading outage. **Upgrade note:** a new optional `[metrics]`
  config key; absent means the profile is off, so existing deployments are unaffected.
- **A durable KDS bump survives a restart** (WS-D / #103). Projection-rebuild-on-load already replays
  the log on boot (`pos_edge`'s `rebuild`); a regression test now proves the `kitchen.ticket.bumped`
  event folds back on rebuild, so a kitchen screen coming up after a restart does not re-show a ticket
  the kitchen already made.
- **A kitchen-display bump is now durable and agreed across every screen** (P6 residual / #44). A bump
  used to be UI-local — each KDS held its own "done" set, so a second screen or one that reconnected
  never agreed a ticket was made. `POST /api/kds/bump` now records the durable `kitchen.ticket.bumped`
  event and fans it out, and the edge marks a projection set (`Edge::bumped_line_ids`) so the prepared
  lines survive a rebuild. A bump is orthogonal to a line's order state (a made line is still
  `Fired`), so it is written as an event and folded, not as a state-machine transition. The kitchen
  and expo screens fold the same event, so the bumped line drops off every connected screen at once;
  "All away" on the pass bumps a whole table's lines through the one path. A domain test proves the
  event is written, the projection reflects it, and it reaches the fan-out. **Scope:** connected
  screens agree live; seeding a *late-joining* KDS with the already-bumped set on connect rides on the
  same projection-rebuild-on-resync follow-up the rest of the client's live projection waits for
  (`ui/src/App.tsx`). Forward-only and additive; no migration, no protocol change (the event was
  already in the catalogue).
- **A menu published from the dashboard now reaches a trading store's counter without a restart**
  (ADR-0004 cloud-owned config, ADR-0039 config delivery, WS-B / #101). The edge's `EdgeSession` — the
  price book, tax table, capabilities and locale a command reads — is now held behind an
  `RwLock<Arc<…>>` so it can be swapped live: a reader takes a cheap coherent `Arc` snapshot for the
  whole of its command, and `Edge::apply_session` installs a rebuilt one for the next. A new config-pull
  client long-polls the store's effective config from the cloud, rebuilds the session with the pure,
  forgiving `session_from_config` (it reads the compiled `menu` node the catalog publish writes, and an
  absent or malformed node leaves the price book unchanged — a bad publish never blanks a trading
  store), and hot-swaps it into the running edge. The HTTP is a seam (`ConfigTransport`), so the loop is
  tested with no socket, and an integration test proves a live edge that booted with an empty menu
  picks up a published one on the next pull. The whole 60-test edge domain suite still passes over the
  new session cell, unchanged. **Scope:** this rebuilds and hot-swaps the live session; persisting the
  pulled document to the edge's local `ConfigStore` (so a restart keeps the last menu without a
  round-trip), applying the delta form of an update, and reading tax/capability nodes as they gain a
  published shape, layer on this seam. Forward-only and additive; no migration, no protocol change.
- **The store can now pull its order queue from the cloud and report each outcome back** (ADR-0061,
  P11a-2, edge half). A new `pos-edge` relay client long-polls the cloud's per-store queue, feeds each
  pulled order through the store's own `EdgeOrderIn` (which reprices, opens it in the local log, and
  dedupes on the caller's reference — so a redelivery converges on one order in the kitchen), and acks
  the outcome — an `Accepted` record or a typed refusal — back to the cloud. A malformed payload is
  acked as `invalid_argument` rather than dropped, so the cloud stops re-parking it. The HTTP is a
  seam (`RelayTransport`), so the pull→make→ack loop is tested with no socket against the real
  `EdgeOrderIn`; the wire shapes are re-declared to mirror `pos_cloud::relay` (the edge must not depend
  on the cloud crate) and pinned to the cloud's JSON by a round-trip test. The client and its
  background `run` loop are library-ready; wiring the production TLS transport and spawning the loop in
  `serve()` land with the edge config-pull integration (WS-B) that shares the same plumbing.
  Forward-only and additive; no migration, no protocol change.
- **Guests can order from a table QR code, and the public order API is now in the OpenAPI document**
  (ADR-0057/ADR-0012 QR ordering; ADR-0019/ADR-0056 for the doc; P11a-2). A new guest-facing
  `POST /v1/qr/orders` takes an HMAC-signed table token as its only credential — a guest carries no
  API key — verifies it, weighs the store's QR guardrails (offline, business hours, a per-table rate
  limit, staff-confirmation **on by default**) with the existing pure decision, and on acceptance
  forwards the order into the very same relay `POST /v1/orders` uses, on the QR channel for the token's
  table. The guardrail settings come from the store's `qr` config node (all forgiving defaults); the
  per-table rate limit is an in-process sliding window (the cloud is one VPS, ADR-0003). The endpoint
  is off until a `table_token_secret` is configured, the same way `/admin/setup` is gated — a guest
  order to a cloud with no secret is unverifiable, so there is nothing to serve. Separately, the
  public `POST`/`GET /v1/orders` handlers are now annotated for OpenAPI, so `docs/openapi.json`
  registers them (with the `OrderRequest`/`OrderResponse` schemas) alongside the daily-rollups path —
  the generated document, regenerated in this change, is once again the whole external `/v1` contract.
  `FakeIntake`-tested (a signed token is accepted and awaits staff confirmation; a token signed with
  another secret is refused `403` before intake) plus unit tests for the business-hours window and the
  rate limiter. **Upgrade note:** set `table_token_secret` in the cloud config to turn on QR ordering;
  leaving it unset keeps the endpoint off. Forward-only and additive; no migration.
- **A menu can now be organised into sections** (ADR-0066 entity 7, Phase 2a). A `MenuSection`
  (tenant-scoped, per-menu, archived-not-deleted) carries a name and a sort order, behind
  `/admin/catalog/menus/{menu_id}/sections` (list/create/`PATCH`); a placement names the section it
  sits under via a new nullable `menu_section_id`, editable by name in the menu editor and shown as a
  column in the placement list. Backed by migration `0017` (a `catalog_menu_sections` table plus an
  additive nullable column on `catalog_placements`) and the `PostgresCatalog` adapter;
  `FakeCatalog`-tested (a section round-trips and a placement carries its section back), and the
  dashboard typechecks, i18n-lints (en + vi) and builds. **Authoring-only for now:** the compiled
  `MenuBook` is a flat set of entries with no sections, so a section changes only what the operator
  sees while authoring — never what the edge is served. Forward-only and additive; greenfield.
- **The catalog can now author modifier groups — shared option sets attachable to items** (ADR-0066
  entities 4 and 5, Phase 2a). A `ModifierGroup` (tenant-scoped, archived-not-deleted) carries a name,
  a `min_select`/`max_select` choice range, the **member** items that make up its options, and the
  items it is **attached** to — behind `/admin/catalog/modifier-groups` (list/create/`PATCH`), with
  members and attachments authored by name in the menu editor. Backed by migration `0016` (members and
  attachments stored as JSONB arrays of item ULIDs on a single row — no child tables) and the
  `PostgresCatalog` adapter; `FakeCatalog`-tested for the round-trip of members and attachments, and
  the dashboard typechecks, i18n-lints (en + vi) and builds. **Authoring-only for now:** modifier
  groups are captured and published in the cloud but do not yet ride the compiled `MenuBook` to the
  edge — the wire cannot carry a modifier reference on a `MenuEntry` until `pos-proto`/ADR-0063 gains
  that field, which is a flagged follow-up. Forward-only and additive; greenfield.
- **The catalog now has a presentation tier — display taxonomy + per-channel layouts, compiled and
  published beside the price book** (ADR-0066 entities 11 and 12, Phase 2a). The catalog gains a
  **display taxonomy** (`DisplayCategory` / `DisplaySubcategory` — the grouping a screen shows, distinct
  from the operational item taxonomy) and **layout buttons** (an item's button on a channel, under a
  display category/sub-category, with an optional POS grid slot and sort order), behind
  `/admin/catalog/display-categories`, `/admin/catalog/display-subcategories` and
  `/admin/catalog/layout-buttons/{sales_channel}/{menu_item_id}` (list/create/`PATCH`; the button
  keyed by `(channel, item)` with `PUT`/`DELETE`). A new pure `compile_layout_book` folds the buttons
  by `channel → category → sub-category` into a `pos-proto` `LayoutBook`, and **publish now writes it
  onto the store's `layout` config node** alongside the `MenuBook` on `menu` — so a button moving
  reprices nothing and a price change relays no buttons, exactly the separation ADR-0066 requires. The
  layout compiler is forgiving (a button whose category/sub-category is missing or archived is skipped,
  never failing a publish). Backed by migration `0015` and the `PostgresCatalog` adapter. A new
  back-office **Layout** screen (`/layout`, kept deliberately apart from the Menu/pricing screen) edits
  the display taxonomy and, per channel, the item buttons a screen shows — item, display category and
  sub-category, an optional POS grid slot (column/row) and a sort order — all by name; the layout ships
  to the store on the same publish as the price book. `FakeCatalog`-tested (route round-trips) and the
  compiler unit-tested (channel/category/sub-category grouping, archived-category skip, and the
  publish e2e asserting the `layout` node); the dashboard typechecks, i18n-lints (en + vi) and builds.
  Forward-only and additive; greenfield.
- **Items now carry an operational category and sub-category** (ADR-0066 entities 2 and 3, Phase 2a).
  The catalog gains an item taxonomy — `ItemCategory` and `ItemSubcategory` (tenant-scoped,
  archived-not-deleted, a sub-category nested under a category) — behind `/admin/catalog/item-categories`
  and `/admin/catalog/item-subcategories` (list/create/`PATCH`), plus two additive nullable columns on
  `catalog_items` linking an item to its category and sub-category. Backed by migration `0014` and the
  `PostgresCatalog` adapter. The menu editor's item form gains category + (category-filtered)
  sub-category pickers, an editable Categories and Sub-categories card, and a Category column on the
  items table. This is the taxonomy a product-mix report will total by — **distinct** from the
  presentation taxonomy a screen groups by (that is the display taxonomy, entities 11/12). It is
  authoring metadata today; the reporting that consumes it is a later phase. `FakeCatalog`-tested
  (category + sub-category + item linkage round-trips) and typechecked in the dashboard. Forward-only
  and additive; greenfield, no backfill.
- **Tax classes are now named entities, and an item picks one from a list** (ADR-0066 entity 10, Phase 2a).
  A `TaxClass` (id + name, tenant-scoped, archived-not-deleted) joins the catalog store behind
  `/admin/catalog/tax-classes` (list/create, `PATCH` to rename or set status), backed by migration
  `0013` (`catalog_tax_classes`, RLS by tenant) and the `PostgresCatalog` adapter. The menu editor's
  item form now offers a **tax-class dropdown** instead of a pasted `TaxClassId` ULID — closing the
  one raw-ULID entry the editor still had — and the items table shows the class by name. The class is
  country-agnostic; the *rate* for each `(tax_class, channel)` still lives in the store's locale pack.
  `FakeCatalog`-tested (create/list/rename/archive, `404` on an unknown id) and typechecked in the
  dashboard. Migration is forward-only and additive; greenfield, no backfill.
- **The back-office now has a menu editor** (ADR-0066, Phase 2a). A new **Menu** screen (`/catalog`)
  in the cloud dashboard drives the catalog CRUD and publish routes end to end, all by name: create /
  rename / archive **items** (with their tax class); create / rename / **reparent** (inheritance) /
  archive **menus**; open a menu to edit the **items placed in it** — each with a per-channel price
  sheet (dine-in / takeaway / delivery / QR / API, blank = not sold on that channel) and a published
  availability toggle — and finally **publish** a chosen menu to the store in the top-bar context,
  which returns the new config version. Tenant and store come from the existing context picker (no
  ULID typed for either); prices are entered in the currency's smallest unit. All labels run through
  the i18n runtime (English + Vietnamese, `en` the enforced fallback) so the no-hardcoded-strings gate
  stays green. One known gap, called out in the UI: an item's **tax class** is still a pasted ULID —
  a named tax-class picker is a later slice. This completes Phase 2a's operator-facing surface.
- **A menu now publishes to a store — compiled and written onto its config tree** (ADR-0066, Phase 2a).
  A `catalog_publish_router` adds `POST /admin/catalog/publish` behind the super-admin session guard:
  given `(tenant_id, store_id, menu_id)` it loads that tenant's items, menus and placements, runs the
  pure `compile_menu` to a `MenuBook`, and writes the book onto the store's **Store-layer** `menu`
  config node (index 2 in the Tenant→Brand→Store→Device order), re-publishing that whole layer through
  the versioned config tree — so the price book reaches the store exactly like every other config
  change, no new transport. A compiler refusal (unknown or cyclic menu) surfaces as **422**, an
  operator error to fix, distinct from a store 5xx; a store with no tree yet gets one started. Proven
  end to end against the fakes: authoring an item + a dine-in placement, publishing, then reading the
  store's effective config back and finding the compiled `MenuBook` on its `menu` node. Wired into
  `pos_cloud`. Prices stay a T2 asset — compiled and shipped in config, never logged. This closes the
  Phase 2a authoring→compile→publish path; the menu editor UI is the next slice.
- **The catalog is now editable over `/admin`** (ADR-0066, Phase 2a). A `catalog_router` exposes the
  authoring model behind the super-admin session guard: `/admin/catalog/items` and
  `/admin/catalog/menus` (list scoped to `?tenant_id=`, create with the id minted server-side,
  `PATCH` to rename / set status / reparent), and `/admin/catalog/menus/{menu_id}/placements` (list,
  `PUT` to upsert an item's per-channel prices and availability by its `(menu_id, menu_item_id)` pair,
  `DELETE` to remove it). Tenant named the admin-is-global way; every write is `FakeCatalog`-tested for
  create/list/upsert/remove and the session guard. Wired into `pos_cloud`. The publish path (compile →
  config tree) lands in this release; the menu editor UI is the next slice.
- **The catalog authoring model now persists in PostgreSQL** (ADR-0066, Phase 2a). Migration `0012`
  adds `catalog_items`, `catalog_menus` (with an inheritance edge) and `catalog_placements` (an item
  in a menu, its per-channel prices as a `jsonb` document keyed by `(menu_id, menu_item_id)`), all
  RLS-isolated by tenant like the registry tables. A `store-postgres` `PostgresCatalog` adapter and
  the `pos-cloud` `impl CatalogStore for PostgresCatalog` back the seam end to end: items and menus
  create/list/update, placements upsert/list/remove, prices round-tripping through the `text::jsonb`
  cast the config tree already uses. Forward-only and additive (greenfield — no backfill); prices are
  a T2 pricing model, stored only here and in the compiled config, never a log.
- **The menu compiler turns authoring into the flat per-channel snapshot the edge reprices from**
  (ADR-0066, Phase 2a). A pure `compile_menu(items, menus, placements, root)` in `pos-cloud` folds a
  menu's inheritance chain **most-specific-wins** (a child's placement overrides an ancestor's,
  untouched items inherited) and selects each placement's per-channel price into that channel's
  `MenuCatalog`, producing a `MenuBook`. Archived items are omitted; an unavailable placement compiles
  to a **present-but-86'd** entry (the published-availability floor); an unknown item, a missing menu,
  or an inheritance cycle is refused with a named `CompileError`, never substituted. Output is
  deterministic (channels by wire token, entries by item id), so re-compiling unchanged authoring
  yields a byte-identical snapshot and the config tree ships a new version only on real change. This
  is the keystone of Phase 2a — where the authoring model actually becomes the price book the store
  reads. (Wiring it to a store's menu assignment and publishing it to the config tree are later
  slices.)
- **The cloud now has a catalog authoring seam — the source of truth a menu compiles from**
  (ADR-0066, Phase 2a). `pos-cloud` gains a `CatalogStore` trait and its records for the item master,
  menus (with a `parent_menu_id` inheritance edge), and menu placements (an item in a menu, with its
  per-channel prices and a published-availability floor). It is deliberately distinct from the config
  tree (which carries only compiled output) and the registry (identity/naming), mirroring how
  ADR-0065 separated those — tenant-scoped create/list/update, entities archived not deleted, an
  in-memory fake proving the contract. A menu id is a cloud-authoring concept that never crosses the
  wire, so `MenuId` is defined beside the seam like `BrandId`. No Postgres or admin routes yet (later
  slices); this is the model the compiler reads to produce a `MenuBook`. Prices are a T2 asset —
  authored and compiled in the cloud, never logged.
- **The compiled menu now carries a per-channel presentation plan, separate from its prices**
  (ADR-0066, Phase 2a). `pos-proto` gains `DisplayPlan` (display categories → sub-categories →
  item buttons, each with an optional POS grid position) and its channel-keyed `LayoutBook` — the
  layout twin of `MenuBook`, delivered on a separate `layout` config node so a button moving reprices
  nothing and a price change relays no buttons. The display taxonomy (`DisplayCategoryId`/
  `DisplaySubcategoryId`) is deliberately distinct from an item's operational category: a screen can
  group "Summer specials" while those items report under "Pizza". `LayoutBook::plan_for(channel)` is
  total (unconfigured channel → empty plan), and `pos-core` never reads any of it — only the POS /
  tablet / QR / marketplace UI does. Additive and forward-compatible.
- **The compiled menu can now price per sales channel** (ADR-0066, Phase 2a). `pos-proto` gains a
  `MenuBook` — a channel-keyed set of `MenuCatalog`s plus a fallback — so the same item can be one
  price dine-in and another on delivery without touching the tested `MenuEntry`/`reprice_line`
  contract: the cloud resolves the channel at compile time and the edge selects the right catalog
  with `MenuBook::catalog_for(channel)` (total — an unpriced channel gets an empty catalog and refuses
  every line, never guesses). This is the first `pos-proto` shape of the cloud catalog whose design
  ADR-0066 fixed; the compiler that fills it and the edge wiring that reads it land in later slices.
  Additive and forward-compatible (the fallback is `#[serde(default)]`); `MenuCatalog`/`MenuEntry`/
  `TaxRateTable` are unchanged.
- **The new-store wizard now hands the operator a ready-to-run `config.toml`, and a runbook ties store
  provisioning together end to end** (ADR-0065, WS-C #102). The wizard created a store and issued its
  API key but stopped short of the one file the store machine actually needs. Its handoff step now
  generates the edge's bootstrap `config.toml` — the store's `store_id` as the one active key, with the
  store/tenant names and this cloud's URL as comments — and offers **Copy** and **Download config.toml**.
  The file is deliberately minimal: the edge's config schema is `deny_unknown_fields` and carries only
  identity, because a store server gets its credential by activation, never from a file on disk
  (ADR-0004, ADR-0051) — so the generator emits exactly what the binary accepts and nothing that would
  make it refuse to start. A new guide, **[Bring a store online](docs/guides/bring-a-store-online.md)**,
  walks the whole path: create the store → download `config.toml` → install the service → sell offline →
  activate devices → publish configuration, honest about which steps compose into the shipping binary
  today. Also: the device-proposals screen now shows a proposal's store by its **registered name**
  instead of a raw ULID, resolved against the registry. Bilingual EN/VI, behind the no-hardcoded-strings
  lint.
- **Device activation now picks (or adds) a device by name — the last raw-ULID entry point is gone**
  (ADR-0065, WS-C #102). The Activation screen asked the operator to type a `Device slot ULID`, the
  final place in the dashboard that still demanded a hand-typed identifier. It now **loads the store's
  devices from the registry** (`GET /admin/stores/{store_id}/devices`) and lets the operator choose one
  by name and kind, or **add a device** inline (name + a kind picker: POS terminal, printer, kitchen
  display, tablet) over `POST /admin/stores/{store_id}/devices`, before issuing the one-time activation
  code (still shown once). Both cards are scoped to the picker's tenant and store and sit behind the
  super-admin session. Bilingual EN/VI, behind the no-hardcoded-strings lint. This completes WS-C's goal
  of removing every ULID-entry field from the operator-facing dashboard.
- **A guided new-store wizard onboards a store from zero without a ULID** (ADR-0065, WS-C #102). A
  three-step flow at `/stores/new` (linked from the Stores screen): name the store and optionally put
  it under a brand → it is created in the registry → issue the scoped API key its devices use to reach
  the cloud (shown once) → a handoff summary with the store's id and the next steps (activation,
  configuration). It composes the registry and API-key admin routes; the tenant comes from the picker.
  Bilingual EN/VI, behind the no-hardcoded-strings lint. Device activation from inside the flow is the
  next slice.
- **A Stores & brands management screen names the stores the registry backfilled** (ADR-0065, WS-C #102).
  The backfill (migration `0011`) surfaced every existing store under a placeholder name like
  `Store 01J9…`; there was no way to fix that or to add a store from the dashboard. A new **Stores**
  screen (behind the super-admin session, scoped to the picker's tenant) lists the tenant's stores,
  **renames** them, **creates** new stores and brands, **assigns** a store to a brand, and
  **archives / restores** — all by name, over the registry's `/admin/stores` and `/admin/brands`
  routes. Bilingual EN/VI, behind the no-hardcoded-strings lint. The guided create-store **wizard** and
  device naming are the next slices.
- **The dashboard now picks the tenant/store by name, not by typing a ULID** (ADR-0065, WS-C #102).
  The top bar's two free-text `Tenant ID` / `Store ID` boxes — the ones that leaked
  `tenant_id is not a ULID` and made the screen unusable for a non-technical operator — are replaced
  by a **context picker** that reads the new registry (`GET /admin/tenants`, `/admin/stores`) and
  lets the operator choose a tenant, then one of its stores, by name; the ULID sits underneath, muted,
  for reference only. Every screen still reads the same id in context, so nothing downstream changes —
  it is just chosen from a list now instead of typed. Bilingual EN/VI, behind the no-hardcoded-strings
  lint. The create-store wizard and the full CRUD screens are the next slices.
- **The cloud now has an org registry — named Tenant/Brand/Store/Device** (ADR-0065, WS-C #102). The
  cloud has always addressed a store by two opaque ULIDs; nothing recorded that a tenant, brand, or
  store *exists*, what it is *called*, or which brand and tenant a store *belongs to*. A new
  `store-postgres` migration (`0011`) adds `tenants`, `brands`, `stores`, and `devices` tables —
  each a named, status-bearing row with its parentage, RLS-isolated by tenant exactly as
  `config_trees` is — and a `RegistryStore` seam with `/admin/tenants|brands|stores` (and
  `/admin/stores/{id}/devices`) CRUD behind the super-admin session. Identity and naming live here;
  configuration keeps living in the config tree (ADR-0033), and a store shares its `store_id` between
  the two. `devices` is the canonical device identity that `device_proposals`/`device_credentials`
  key to, not a fourth copy. This is the backbone that lets the back-office dashboard (ADR-0060)
  replace free-text ULID entry with named pickers and a create-store flow — so a normal operator
  never sees or types a ULID, and `tenant_id is not a ULID` stops being reachable. **Upgrade note:**
  migration `0011` runs on boot and **backfills** the registry from existing `config_trees` rows —
  every already-configured `(tenant_id, store_id)` becomes a tenant and a store under a placeholder
  name (`Store <short-ulid>`), idempotently, touching nothing in `config_trees`; renaming them is a
  non-blocking follow-up. No config, protocol, or permission identifier changes.
- **The intake idempotency ledger is now durable and written in the order's own transaction**
  (ADR-0064). `IntakeLedger` is promoted to a `pos-ports` port (a `Transactional` one, like
  `ConfigStore`): `record` buffers the `(sales_channel, external_reference) → record` row into the
  caller's transaction, so it and `sales.order.opened` **commit together or not at all** — a crash
  between opening an order and recording it can no longer let a retry open a second one. `store-sqlite`
  implements it against an `intake_ledger` table (migration 0004); the fake implements it in memory;
  both pass the shared `OrderIn` contract suite. The key is a **plain** insert, so a second order
  racing in on the same reference fails its commit with `already_exists` (the one writer thread
  serialises the two) and rolls back rather than duplicating — `EdgeOrderIn` resolves the loss by
  looking the winner up. A store-sqlite test proves a recorded key survives reopening the database
  and that a duplicate key is refused. With this, **nothing in the order-intake path is in-memory
  any more** — both the dedupe ledger and the daily queue counter are durable. `EdgeOrderIn` drops
  its injected ledger generic (`EdgeOrderIn<S, Q>`): the store `S` now supplies both the event log
  and the ledger, which is what lets them share one transaction.
- **The edge implements `OrderIn`** (ADR-0064) — the store side of order intake. `EdgeOrderIn` reprices
  each inbound line from the store's synced menu catalog (ADR-0063), opens a **tableless** order in the
  local event log (`sales.order.opened` + `sales.order_line.added` in one transaction), and dedupes on
  the caller's `(sales_channel, external_reference)` through a per-store intake ledger — so a
  marketplace's retry or the relay's at-least-once delivery converge on one order in the kitchen. The
  acceptance total is the store's own menu total (tax-inclusive); an unknown item is refused
  (`invalid_argument`), never substituted; a QR order (one that names a table) awaits staff
  confirmation while a delivery/public-API order does not; and it accepts **offline**, since the menu
  is local config and the order is a local log write. Proven against the shared `OrderIn` contract
  suite — the same suite `FakeIntake` passes — so "the edge is a real `OrderIn`" is verified, not
  asserted.
- **A tableless order now gets a durable, daily-resetting queue number** (ADR-0064) — the number the
  counter shouts a takeaway customer back by. A `QueueNumberAuthority` (the same in-memory-vs-`SqliteStore`
  split as `ReceiptAuthority`) is injected into `EdgeOrderIn`; the `store-sqlite` implementation
  (migration 0003, a `queue_counter` keyed by `(store, business_date)` plus an order-keyed
  `queue_allocations` idempotency table) **resets each trading day with no midnight job** and is
  **durable** — a store-sqlite test proves the sequence survives reopening the database, so a box that
  lost power mid-service does not reissue `#1` at a second customer. Allocation is idempotent by order,
  and a QR order (one that names a table) still gets none. The intake **ledger’s** durable SQLite
  backing (in the order’s own transaction) remains the one flagged follow-up; the in-memory ledger
  behind the trait + contract suite ships here.
- **The store menu catalog — the store server's authoritative price book** (ADR-0063), the missing
  piece a real edge `OrderIn` needs. The dine-in path prices on the device, but an inbound order
  (marketplace, `POST /v1/orders`, QR) arrives as identifiers and quantities with no device to price
  from, so the `OrderIn` contract's "the store's price wins" and "an unknown item is refused, never
  substituted" have nothing to stand on. A new `MenuCatalog`/`MenuEntry` (in `pos-proto`, alongside
  the other config shapes) is a per-item price book — `unit_price`, `tax_class_id`, `available` — that
  the cloud publishes under the config tree's `menu` node and the store syncs like any other config.
  A pure `pos_core::menu::reprice_line` turns `(menu_item_id, quantity, modifiers)` into a priced line
  (base + modifier prices, extended by quantity, taxed via the existing channel-keyed `TaxRateTable`),
  or refuses it: an unknown item or modifier (`invalid_argument`), an 86'd item (`failed_precondition`),
  or a class with no rate on the channel (`failed_precondition`, never a silent zero). The caller's
  quoted price is compared and flagged `repriced`, never charged. No consumer yet — the edge `OrderIn`
  that reprices from it is the follow-up PR; this lands the price book and the repricing law, both
  property-tested.
- **The cloud→store order relay** (ADR-0061), so `POST /v1/orders` answers in the binary. Inbound
  orders (marketplace, the public API, QR) are held in a durable per-store `order_queue` and the
  store **pulls** them over its own outbound sync channel — no cloud→store push, so "stores dial out
  only" (4G/CGNAT with no port-forward) is unchanged. The cloud implements `OrderIn` over the queue:
  `submit` enqueues idempotently on `(tenant, store, channel, reference)` and parks up to the store's
  configured deadline, returning the store's real acceptance (`201`/`200`) if it arrives or `503`
  on timeout **with the order still queued**; `look_up` (`GET /v1/orders`) resolves a timed-out
  caller. The store pulls (`GET /sync/stores/{id}/orders`, a bounded long-poll) and reports outcomes
  (`POST /sync/stores/{id}/orders/{queued_id}/ack`) under a new deny-by-default `relay_orders` scope.
  Per-store `store.order_relay.{enabled,wait_ms}` is published from the cloud through the config tree,
  so intake is toggled and tuned per store from the dashboard with no deploy. Validated against the
  in-memory fakes (queue → pull → ack → look-up, idempotency, scope enforcement); `store-postgres`'s
  `order_queue` table is proven by its own integration suite.
- **A back-office dashboard, embedded in `pos_cloud` and served at `/`** (ADR-0060). A new
  `dashboard/` SolidJS + Tailwind SPA — built like the edge `ui/` (shared design tokens, the ICU
  i18n runtime with `en` the enforced floor and a `vi` catalogue, a typed client, the
  no-hardcoded-strings lint) — gives the cloud its first screen. `pos_cloud` embeds it with
  `rust-embed` and serves it as the router's fallback (`crate::assets`), so the API routes match
  first and everything else resolves to the SPA; a `build.rs` writes a placeholder on a fresh
  checkout, exactly as `pos_edge` does. Screens cover the existing `/admin` surface: super-admin
  login + mandatory TOTP and first-run setup, the **config-tree publish editor** (publish a
  tenant/brand/store/device level and view the effective config — the operator half of config
  delivery), API-key provisioning, the printer/KDS approval queue, the translation grid, webhook
  registration, activation codes, and a daily-rollup report. Auth is the existing super-admin
  session cookie unchanged; the SPA holds no secret. A `dashboard` CI job type-checks, lints and
  builds it; `deploy/Dockerfile` gains a Node stage that builds `dashboard/dist` natively (no
  emulation) and embeds it. (ADR-0060)
- `GET /admin/stores/{store_id}/rollups/daily` — the daily rollup read under the super-admin
  session, naming the tenant with `?tenant_id=` (ADR-0060). The `/v1` rollup read stays bearer-authed
  and tenant-scoped by the key; this admin read reuses the same `RollupStore` for the dashboard,
  following the same admin-is-global pattern the config read already uses. Read-only.
- The specification set is now in the repository: `docs/`, ADRs 0001–0012, `AGENTS.md`,
  `CONTRIBUTING.md`, `SECURITY.md`, `MAINTAINERS.md`, `CODEOWNERS`, the GitHub templates,
  and the frozen Vietnamese design archive.
- `LICENSE` — proprietary, internal use, as decided in ADR-0009. It was referenced by
  `README.md` and the ADR but had never been written.
- `docs/roadmap.md` — the dependency-ordered build plan from an empty repository to a pilot
  store, with an exit criterion per phase and no calendar dates.
- ADR-0013 (sans-I/O domain core, async ports, static dispatch), ADR-0014 (date, time and
  timezone library), ADR-0021 (the sixteen ports, superseding 0006), ADR-0024
  (`PROTOCOL_VERSION` negotiation), ADR-0026 (port shapes: one `PortError`,
  `Transactional`/`TxContext`, outbox cursor ordering, fault injection on the harness, and
  three corrections to ADR-0013).
- `pos-ports`: all sixteen ports from ADR-0021. `PortError` carries an AIP-193 status and the
  `PortName` that produced it, so retry policy and the error mailbox need no per-port
  translation. `Transactional` is a supertrait of `EventStore` and `ConfigStore`, so an
  adapter implementing both has exactly one transaction type and "the outbox row commits with
  the state change" is the only thing that type-checks. Object-safe mirrors
  (`DynPrinterDriver` and four others) cover the families selected by configuration rather
  than at compile time.
- `pos-contract-tests`: a shared suite per port, 104 cases in all, parameterised by a harness the
  adapter supplies. Suites are emitted by a macro that takes the adapter's own `block_on`, so this
  crate depends on no async runtime. Destructive operations — losing power, severing a link, staging
  an ambiguous card result — live on the harness rather than on the ports, so no production adapter
  ships a way to corrupt itself. A test asserts every port has a suite.
- `pos-fakes`: in-memory implementations of all sixteen ports, with fixed-capacity queues returning
  `RESOURCE_EXHAUSTED`, and `tests/contract.rs` running every suite against them. `pos-core`'s tests
  will run against these fakes, so this is what stops the domain suite resting on an unchecked
  assumption about how the real store behaves.
- ADR-0027 and `pos-country`: a country module is a **bundle** — a `Fiscalization` implementation, a
  locale pack, tax-code format validation, a default retention period — living at `countries/<cc>/`
  rather than filed among the adapters. Selection is a Cargo feature per country, so a fork serving
  one country edits one line, deletes nothing, and compiles nothing else. `pos-proto` gained
  `CountryCode`, `TaxRate` in basis points, the channel-keyed `TaxRateTable` and `LocalePack`.
- `countries/zz/`: a reference country module that **passes the whole `Fiscalization` contract
  suite**, so a real country fills in a proven shape. `cargo xtask countries` fails a module that is
  misnamed, absent from the workspace, or wired into no binary's features.

- ADR-0025 (receipt-number authority is configuration: gapless only while one store authority is
  reachable), ADR-0028 (the real settlement invariant — `sum(applied) == total_due`, tips a separate
  ledger, cash rounding as an explicit line, tax per tax-class subtotal, service-charge taxability as
  config), and ADR-0029 (line merge: terminal states win, other fields last-writer-wins on
  `(event_time, device_id)`, commutative and associative). These resolve issues #8, #9 and #10 ahead
  of the P3 domain code they govern; `pos-spec.md` §3/§5/§14 and `architecture.md` §2/§5 are amended
  to state the rules the code will enforce.

- `pos-core` begins: the state-machine framework and the five lifecycles from `architecture.md` §5
  — Table, Order, order line, Bill, Shift. Transitions are data, enumerable at runtime, so
  `docs/state-machines.md` is generated from the code (a test keeps it in sync) and one generic
  checker proves every machine exhaustively: no reachable undefined state, no orphan or deadlocked
  state, terminal states with no exit, and a merge that is commutative, associative and
  terminal-preserving. That merge is the ADR-0029 rule — `VOIDED` outranks every editable state, so
  a concurrent edit can never resurrect a voided line — and `bill:settle` being one-time (§14.4) is
  now a property of the `Bill` machine rather than of a lock.

- `pos-core` billing: `assemble` computes a bill's totals with tax **per tax-class subtotal**
  (rounded once per class, the per-class lines reconciling to the tax total a VAT invoice prints),
  bill-level discounts and comps allocated proportionally across classes, a service charge that is
  taxable per config, and cash rounding materialised as an explicit `rounding_adjustment` line —
  ADR-0028 made mechanical. `settle` proves the settlement invariant (`sum(applied) == total_due`,
  change `= tendered − applied − tips`, tips a separate ledger) and refuses under-application and
  negative change. `split_evenly`/`split_by_weights` bind the §14.3 law (`sum(splits) ==
  original_total`) at the domain level. A `DomainError` names which rule the inputs broke. Property
  tests over arbitrary amounts, parts, weights and payments bind §14.3 and §14.5 in CI.

- `printer-escpos` (P4): the ESC/POS thermal-printer adapter for the `PrinterDriver` port. It encodes
  a `PrintDocument` into ESC/POS bytes (text with emphasis/size/alignment, raster bitmaps, CODE39
  barcodes, QR codes, feed, cut) and pushes them at a `Transport`; it does not decide text-vs-bitmap
  (the framework already did, from the code page this adapter reports — ADR-0026 §5). It is idempotent
  by `job_id` (a flaky cable's retry prints one ticket), refuses to open a drawer on anything but USB
  (port 9100 has no authentication), returns `unavailable` when unreachable so the caller re-queues,
  and `failed_precondition` when out of paper. Passes all **8** `PrinterDriver` contract cases via an
  in-memory recording transport. The retry queue and backup-printer failover live at the caller (port
  §2), and the real USB/serial/TCP transports plus the real-print test land with A5 hardware.

- `cargo xtask migrations` (P4, ADR-0017 enforcement): the additive-only gate. It refuses a pull
  request that edits a migration already shipped on the base branch (a migration is immutable — the
  same removal-gate mechanism `xtask snapshot` uses) or that adds a destructive statement
  (`DROP TABLE`, `DROP COLUMN`, `RENAME`) without the reviewed `-- migrations:allow-destructive`
  marker. Wired into the PR workflow and the `just migrations-check` recipe; the destructive
  detection is unit-tested, and the shared git-diff helpers are factored into `xtask::checks` so the
  snapshot and migration gates share one implementation.

- `store-sqlite` (P4): the edge `EventStore` and `ConfigStore` over SQLite, the first adapter. One
  `rusqlite` connection owned by a dedicated writer thread (ADR-0015); the async port methods send a
  command over a bounded channel and await a oneshot reply, so blocking SQLite never touches the
  executor and every write serialises through one point. The outbox position is an AUTOINCREMENT
  rowid assigned inside the commit transaction — monotone, never reused after an acknowledged delete,
  and starting at one so it never collides with `OutboxPosition::START`. A transaction buffers its
  writes in the `SqliteTx` and flushes them in one `BEGIN IMMEDIATE`…`COMMIT` on commit, so a dropped
  handle rolls back and a crash mid-transaction loses only the uncommitted work. Idempotency is
  `INSERT OR IGNORE` (the stored copy wins), reads come back ordered by `event_id`, and the schema is
  WAL with `synchronous = NORMAL`. It passes **all 19** shared contract cases — the same suites the
  fake runs — including both power-loss cases, driven by reopening the database file. The schema is
  the forward-only migration `0001_event_store.sql` applied by the ADR-0017 runner.

- ADR-0015 (SQLite access at the edge: `rusqlite` behind one dedicated single-writer thread, bridging
  blocking SQLite into the async `EventStore`/`ConfigStore` ports over a channel, so gapless outbox
  positioning and `TxContext`-by-shape fall out for free) and ADR-0017 (migrations: forward-only,
  additive, numbered SQL files with a tiny runner and a `cargo xtask migrations` gate that refuses
  editing a shipped migration or a destructive statement). Both block P4 and are merged ahead of the
  `store-sqlite` code they govern.

- ADR-0018 (edge HTTP/WebSocket stack): `pos_edge` serves its UI-facing HTTP over **axum** on
  `hyper` + `tower`, opens WebSockets with axum's built-in `ws` extractor, and fans state changes out
  to every device on the store LAN over a single bounded `tokio::sync::broadcast` channel — an
  in-process send, so the under-50 ms budget is met by construction and a stalled device degrades
  itself (a `Lagged` resync) rather than growing server memory. The SolidJS UI is compiled into the
  binary with `rust-embed` so the store is one static file (a `dev-ui` feature reads from disk
  instead); tokio, axum, tower and rust-embed enter at the binary layer only, and the dependency-rule
  test keeps them out of `pos-core`. This governs the edge's internal transport only — the cloud's
  public `/v1` API and its OpenAPI are ADR-0019 (P7). Merged ahead of the P5 `pos-edge` code.

- ADR-0030 (edge discovery, pairing, offline auth): the always-works discovery path is a QR code
  carrying a raw-IP URL plus manual `IP:port` entry (no name resolution to fail on), with a DHCP
  reservation pinning the IP; mDNS `pos.local` is a convenience behind an `Advertiser` trait whose
  real multicast implementation lands with hardware (like the printer's transports), so no mDNS
  dependency enters the framework now. Device pairing is a single-use, five-minute 6-digit code from a
  vetted CSPRNG. User authentication is a PIN verified offline against cloud-synced Argon2id hashes,
  with a five-failure/five-minute lockout enforced locally in `pos_edge` — the Argon2id cost plus the
  lockout, not PIN entropy, is the brute-force defence. Device tokens and employee ids are logged;
  PINs, hashes, and pairing codes never are. Adds `getrandom` and `argon2` at the binary layer only.
  Merged ahead of the P5 code it governs.
- `pos-edge` (P5): the store binary begins, as a library plus a thin `main` so the HTTP surface is
  testable without binding a socket. It boots an **axum** server (ADR-0018) that answers a `/healthz`
  probe — status, version, protocol version, store id, and no PII — and serves the operator UI, which
  is compiled into the binary with `rust-embed` so the store is one static file (a `dev-ui` feature
  reads `ui/dist` from disk instead). An unknown path falls back to `index.html` for the P6
  single-page app rather than 404ing. Bootstrap config is TOML with `deny_unknown_fields` and a
  required `store_id`; `tracing` is configured in one place with the no-PII rule stated; the server
  drains in-flight requests on Ctrl-C or `SIGTERM`. `ui/dist` is gitignored build output, so a
  `build.rs` writes a placeholder `index.html` there when P6 has not yet built the real one (and
  never overwrites a real build). The binary wires the reference country module (`country-zz`,
  ADR-0027) as a Cargo feature and validates the registry at start-up, logging which countries it can
  serve. The async runtime and HTTP stack enter at the binary layer only — `cargo xtask deps-rule`
  proves they never reach `pos-core`.
- `pos-edge` WebSocket fan-out (P5, ADR-0018): a `/ws` endpoint gives each device one socket fed by a
  single bounded `tokio::sync::broadcast` channel. When the edge applies a change it publishes once
  and every device receives it — an in-process send, so the under-50 ms LAN budget is met by
  construction. The channel is bounded (`FANOUT_CAPACITY`): a device that falls behind is told to
  reload a fresh snapshot (`ServerMessage::Resync`) rather than making the server buffer without
  limit — the same bounded-memory discipline the SQLite writer uses. `ServerMessage` is
  `#[non_exhaustive]` with an internal `type` tag, so a client dispatches on one field and tolerates
  message kinds added later. An integration test binds a real port and proves a published event
  reaches one connected device, and that two devices on one table both receive the same change.
- `store-sqlite` gapless receipt numbers (P5, ADR-0025): the `store_server` authority. A new
  additive migration (`0002_receipt_counter.sql`) adds a per-store counter and a per-bill allocation
  table, and `SqliteStore::allocate_receipt_number` hands out the next number in one `IMMEDIATE`
  transaction. Because every allocation funnels through the one writer thread, the sequence is
  gapless and collision-free even when two cashier devices settle at once; a test drives 200
  concurrent allocations and asserts the result is exactly `1..=200`. Allocation is idempotent by
  `bill_id` — a retry after a crash reuses the number rather than skipping one — and survives
  reopening the database. This is the store's receipt number, never a legal invoice number (the
  country module's, from a pre-allocated range); the two are deliberately never conflated.
- `pos-edge` offline PIN authentication (P5, ADR-0030): `auth::verify_pin` checks a PIN against a
  cloud-synced Argon2id PHC hash with no network, and `auth::Lockout` is the five-failure/five-minute
  lockout — a pure state machine over `(employee, verified, now)`, so the window is unit-tested in
  microseconds against a fixed clock rather than by waiting. A correct PIN while locked out is still
  refused (the lockout must be served); the window lifts and the count resets after five minutes; a
  malformed stored hash is never a way in. PINs and hashes are secrets and never logged; only the
  employee id and the outcome are. Adds `argon2` at the binary layer, and a dated `rand_core@0.6`
  deny skip for the salt-generation line argon2 pulls but the edge (verify-only) never uses.
- `pos-edge` device pairing and discovery (P5, ADR-0030): the edge mints a single-use, five-minute
  6-digit code from the OS CSPRNG (`getrandom`) and shows the operator a raw-IP pairing URL
  (`http://<ip>:<port>/pair?code=NNNNNN`) — the discovery path that needs no name resolution; a device
  redeems the code at `POST /api/pair` for a 128-bit device token. mDNS `pos.local` is a convenience
  behind an `Advertiser` trait whose real multicast implementation lands with hardware (a
  `NoopAdvertiser` default ships now, like the printer's placeholder transports). `SystemClock` is the
  edge's one sanctioned reader of the OS clock (the single place `clippy.toml`'s `SystemTime::now` ban
  is lifted); everything time-related, including pairing-code expiry, reads it through `ClockSource`,
  so it is testable against a fixed instant. Config gains an optional `advertised_ip` (the
  DHCP-pinned LAN IP) for the pairing URL. Pairing codes and device tokens are secrets and never
  logged.
- `pos-edge` ULID `IdGenerator` and SNTP drift monitor (P5): `idgen::EdgeIdGenerator` mints
  monotonic, time-sortable ULIDs over a `ClockSource` — it clamps to a non-decreasing timestamp so an
  NTP step backwards cannot emit an id that sorts before one already handed out (the event feed pages
  by ULID), and increments the random component within a millisecond so same-ms ids strictly
  increase. The 80 random bits come from a SplitMix64 stream seeded once from the OS CSPRNG; a ULID's
  randomness is not a secret (pairing codes and device tokens take OS entropy directly). `sntp::assess`
  is the pure drift decision — an offset past two seconds from a reference clock alarms, because the
  business date is derived from the store's local time and a drifting clock files sales under the
  wrong day. The SNTP network poll that feeds it lands with deployment, like mDNS.
- `pos-edge` config hot-reload and service units (P5): `active_config::ActiveConfig` swaps the running
  configuration atomically and in well under a second, retaining the previous good version so a
  change that turns out wrong rolls back one step, and refusing a candidate that fails validation
  without touching the active config — a bad config cannot brick the store. Reads take a short read
  lock and clone an `Arc`, so a handler reading config to answer a screen never blocks on a writer;
  content validation against the config schema is generic (the schema is P7). `deploy/edge/` adds a
  hardened systemd unit and a Windows service guide; both deliver the `SIGTERM`/stop the binary
  already drains gracefully, so a committed sale is durable and an interrupted one was never
  acknowledged.
- `pos-edge` application layer (P5 keystone): `app::Edge<S>` is the load → decide → apply → publish
  loop ADR-0013 gives each binary. For a command it loads the aggregate's state from an in-memory
  projection, decides with the synchronous `pos-core` spine, writes the wire events it maps to inside
  one store transaction, and — only after the commit — folds the change into the projection and
  publishes it to every device over the fan-out, so a rolled-back write is never shown. It is generic
  over the store `S`, so the identical loop runs against `pos-fakes` in a test and `store-sqlite` on a
  real machine (static dispatch, no `dyn`). This slice wires the table floor cycle (seat →
  `sales.table.opened`, clean → `sales.table.closed`); the order, bill and shift families follow the
  same shape. Tests prove seating opens a table, that two devices both see the change over the
  fan-out (the dine-in exit criterion in miniature), and that an illegal transition is refused and
  publishes nothing. `StoreIdentity` and `EdgeSession` carry the envelope context and the
  config-driven decision inputs.
- `pos-edge` HTTP domain routes (P5): the table floor cycle is now reachable over HTTP —
  `POST /api/tables/{id}/seat`, `POST /api/tables/{id}/clean`, `GET /api/tables/{id}` — each a thin
  shell over the application loop that returns the table on success, `409 Conflict` for an illegal
  transition (the caller's fault, not the server's), and `400` for a non-ULID id. `serve` is now
  generic over the store and composes the domain router with the infra router, **sharing one fan-out**
  so a committed change over HTTP reaches every `/ws` device. The real `pos-edge` binary composes
  `store-sqlite` (with a `store_path` config key), and `examples/minimal-edge` composes `pos-fakes`;
  `StoreIdentity::for_store` and `EdgeSession::bootstrap` supply the envelope context and
  config-driven decision inputs until the cloud config tree (P7). An integration test drives seat →
  read → clean and the 409/400 paths without a socket. The acting actor is a fixed development
  identity pending token→actor resolution.
- `pos-edge` order-line flow (P5): `Edge::add_line` records a line on the order a table holds
  (`sales.order_line.added`) and `Edge::fire_line` sends it to the kitchen
  (`sales.order_line.fired`) through the `pos-core` `decide_line` spine, consuming its recipe (§8).
  The edge does not invent prices: a `LineDraft` carries the amounts the device captured from the
  menu it holds (`unit_price`, `line_total`, `tax_class`, `tax_rate` — a line never references the
  live menu), and the projection remembers each line's item, quantity and state so a fire can be
  decided and (once the menu's bill of materials syncs, P7) its consumption computed. Firing an
  already-fired line is refused by the state machine; adding to an unseated table is refused. The
  `commit_and_publish` half of the loop is now generic, shared by every command. The bootstrap
  session carries an empty `RecipeBook` (an unrecipe'd item consumes nothing) until P7.
- `pos-edge` bill flow (P5, ADR-0025/ADR-0028): `Edge::open_bill` opens a bill on the order a table
  holds (`billing.bill.opened`) and moves the table to awaiting payment; `Edge::settle_bill`
  assembles what is owed from the order's captured line totals (`billing::assemble`, tax per
  tax-class subtotal), proves the payments sum **exactly** to it (`decide_bill` → `billing::settle`),
  allocates the gapless per-store receipt number for that bill, then appends
  `billing.bill.settled` carrying the number and the subtotal/reduction/service-charge/tax/rounding
  breakdown, and cycles the table to needs-cleaning. A split tender (cash + card) that sums to the
  total settles; an underpayment, a second settle, and a bill on an unseated table are each refused.
  The `Effect::PrintReceipt` is returned on the `BillView` for the caller to run after commit — the
  edge holds no printer, so a rolled-back settle prints nothing.
- `receipt::ReceiptAuthority` (P5, ADR-0025): the gapless receipt-number authority is injected into
  the generic `Edge<S>` rather than derived from its store type. The real binary passes the
  `SqliteStore` itself (its single writer thread is the authority); `receipt::InMemoryReceipts` is the
  same gapless, bill-idempotent contract without a database, for the example and the engine tests.
  This is the store's receipt number, never a legal invoice number.
- `EdgeSession` now carries the store's channel-keyed `TaxRateTable` and default `SalesChannel` (D6),
  so a bill assembles a real total offline. The bootstrap rates one standard class
  (`EdgeSession::standard_tax_class`) at 10% dine-in until the cloud config tree (P7) supplies the
  menu's classes; Vietnam v1's single rate is a special case of the same two-dimensional table.
- `pos-edge` cash-shift flow (P5, §6/§11.1): `Edge::open_shift` opens a shift with a starting float
  (`cash.shift.opened`), `Edge::count_shift` records the **blind** count (`cash.shift.counted`) —
  returning no expectation or variance, so the cashier counts before the system reveals what it
  expected — and `Edge::close_shift` reveals the expected drawer cash (opening float plus the cash
  its bills took) and the variance (`cash.shift.closed`), surfacing `Effect::PrintShiftReport` for
  the caller. Only cash tenders roll into the expectation; card sales, tips and cash rounding never
  touch the drawer. One shift is open per device: a second open is refused, and every event minted
  while a shift is open now carries its `shift_id`. A close that skips the count is refused by the
  state machine.
- `pos-edge` order, bill and shift routes over HTTP (P5): the whole sell cycle is now reachable —
  `POST /api/tables/{id}/lines` and `POST /api/lines/{id}/fire` (order), `POST /api/tables/{id}/bill`
  and `POST /api/bills/{id}/settle` (bill), and `POST /api/shifts`, `POST /api/shifts/{id}/count`,
  `POST /api/shifts/{id}/close` (shift). Each is a thin shell over the application loop, sharing one
  error mapper (a refused command is `409`, an unreachable store `503`, a non-ULID id or unknown
  payment method `400`). A payment method arrives as an `Open` enum, so an unrecognised token is a
  clean `400` rather than a deserialise failure, and the domain boundary refuses an unspecified one.
  An integration test drives a table seat → line → fire → open bill → settle (gapless receipt) →
  clean, and a shift open → blind count → close, entirely over the router without a socket.
- `pos-edge` records each tender as a `billing.payment.captured` event (P5): a settle now appends one
  captured-payment event per tender **and** the `billing.bill.settled` event in a single transaction,
  so a crash never leaves a receipt without its payments. The captured payments are what let the
  shift cash roll-up be rebuilt from the log; a cash payment's outcome is `CAPTURED`, tips are held
  apart (per-payment tip capture is P7).
- `pos-edge` rebuilds the projection from the durable log at boot (P5 crash recovery, ADR-0015):
  `Edge::rebuild` replays every event in `event_id` order and folds it back — table, order line,
  bill, table cycle, and the shift float-plus-cash roll-up — so a restart resumes exactly where the
  last committed transaction left off and only an *uncommitted* transaction is lost. The `pos-edge`
  binary calls it before serving. Idempotent: replaying committed facts lands on the same state.
  Integration tests prove a second edge over the same store recovers a settled sale, a fired line,
  the cleaned-down table cycle and the shift's cash total, and that a double rebuild is a no-op.
- The **dine-in acceptance flow** is now an automated test (`tests/dine_in.rs`, the P5 exit
  criterion): one table, two devices, no network — seat → both devices order → fire by course → add
  a later course → open the bill → settle it split across cash and card → a gapless receipt → the
  table cycles to clean. Every committed change reaches both devices over the fan-out, and the flow
  runs entirely on the in-memory fakes, which is the offline demonstration. With this, P5 (`pos_edge`)
  meets its exit criteria: a store seats, orders from two devices, fires, settles with a gapless
  receipt and cycles the table, offline throughout, and a kill mid-sale loses only the uncommitted
  transaction (`tests/recovery.rs`).
- `ui/` (P6) — the operator interface begins: a SolidJS + Tailwind app built with Vite and
  TypeScript, embedded into `pos_edge` with rust-embed (ADR-0018). It carries the design-token file
  (spacing, type, touch, radius, motion and colour, in light and dark), integer-minor-unit money
  formatting, a typed client for the edge's routes, and a reconnecting `/ws` live link that folds the
  fan-out into a small client projection — so what one device does appears on every other. The
  primary flow is playable: a floor plan, a table's order (add items, fire), and a pay screen that
  settles for a gapless receipt with the VND quick-cash denominations; a persistent status bar shows
  the store link (offline from the cloud is a normal working state) and the shift. The remaining
  screens (KDS, expo, Today, shift, pairing) and the four device layouts follow. A CI `ui` job
  type-checks and builds the app on every pull request; the Rust build still embeds `build.rs`'s
  placeholder on a fresh checkout, since `ui/dist` is gitignored. Requires Node ≥ 22 and `pnpm`.
- `ui/` (P6) — the remaining operator screens, over the same live projection: the **kitchen display**
  (every fired line, bump to clear) and the **pass** (fired lines gathered by table, "all away"), both
  on a dark theme they take over while open (legible at two metres); **Today** (the floor at a glance
  — table counts, open bills, shift, a live read rather than a report); the **cash shift** (open →
  blind count → close revealing the variance, the count never shown beside the expectation); and
  **pairing** (redeem a six-digit code, pre-filled from the QR link). A status-bar nav links them.
  Layouts are responsive across phone, tablet and POS from one breakpoint set, with the kitchen and
  pass on their own dark treatment. Known follow-ups, called out rather than hidden: the KDS/pass
  bump is a screen-local acknowledgement until a durable "line made" event exists; ICU i18n with an
  `en` fallback and the no-hardcoded-strings CI check are blocked on ADR-0020; the WCAG-AA audit and
  the per-device layout tuning are part of the visual pass.
- ADR-0020 and `ui/` i18n (P6): the interface is internationalised. Messages are ICU MessageFormat in
  per-locale JSON catalogues (`en.json` canonical, `vi.json` a first-pass Vietnamese translation),
  formatted by `intl-messageformat` over the platform `Intl` (no bundled CLDR data — the embedded
  Chromium already carries it), with **`en` the enforced fallback** so a missing translation shows
  English, never a blank. `t(key, args)` reads a reactive locale signal (a language toggle sits in
  the status bar), and `MessageKey` makes a mistyped key a compile error. **No user-visible string is
  hardcoded**: `pnpm i18n:lint` parses every `.tsx` with the TypeScript compiler and fails the build
  on a JSX text node with a letter, or a hardcoded `placeholder`/`title`/`aria-label`/`alt` — proven
  to fire on a probe. The `ui` CI job runs it, so the seventh standing rule (`AGENTS.md` §2) is now a
  merge gate for the UI. Accessibility: native focusable controls with a visible focus ring, no
  meaning by colour alone, `role="alert"` on errors, the document `lang` tracking the locale, and
  ≥48px touch targets; the numeric WCAG-AA contrast audit of the oklch palette is the remaining
  visual-pass item. Adds `intl-messageformat` to the UI's dependencies (the Rust backbone is
  untouched).
- The four P7 decisions, ahead of the cloud code they govern (ADR-before-code): **ADR-0016** — cloud
  PostgreSQL access is `tokio-postgres` behind a `deadpool` pool with hand-written SQL and RLS set per
  transaction, chosen so the workspace builds with no database and correctness is proven by tests
  against a real PostgreSQL rather than by the compiler. **ADR-0022** — the events table is
  range-partitioned **monthly by `business_date`**, tenant isolation is RLS (a column and a policy, not
  the partition key), and retention drops whole partitions; resolves the three-way partition ambiguity
  and supersedes ADR-0008's "by `store_id`" phrasing. **ADR-0023** — tenants are flat per-tenant
  subdomains with no country label, DNS created through the Cloudflare API is the slug-uniqueness
  ledger (no shared cross-cell database), redirect never proxy, wildcard renewals staggered above ~5
  cells; resolves the ADR-0011/archive contradiction and supersedes ADR-0011's country-in-hostname
  mechanism while keeping its redirect principle. **ADR-0019** — the public `/v1` OpenAPI is generated
  from the axum handlers with `utoipa` and a drift gate (a `pos-cloud` test that renders
  `docs/openapi.json` and fails CI on any difference, the same idiom as the event-catalogue snapshot),
  never hand-written. Registered in the ADR index and the engineering guide; these unblock the P7
  schema, adapters and `pos_cloud`.
- `store-postgres` (P7): the cloud `EventStore` over PostgreSQL, and the second real implementation
  of that port — it passes the **same** shared contract suite as `store-sqlite` and the in-memory
  fake, which is what makes "the cloud store behaves like the edge store" a checked fact. Migration
  `0001_cloud_events.sql`: the event log is range-partitioned **monthly on `business_date`**
  (ADR-0022) with a default safety-net partition and a `create_events_partition` function the cloud
  calls ahead of need, idempotent by `(business_date, event_id)` — the partition key must be in the
  primary key, and a replay carries the same business date, so this is `event_id` idempotency in
  practice. Tenant isolation is row-level security on `tenant_id`: a session that has not set
  `app.tenant_id` sees nothing (default-deny), so a query that forgets its tenant returns empty
  rather than leaking across tenants. The envelope is a `json` column, not `jsonb`, because the
  contract requires a replayed event to read back byte-for-byte identical and `jsonb` reorders keys
  and reformats whitespace. Access is `tokio-postgres` behind a `deadpool` pool (ADR-0016) with no
  build-time database; the pool recycles connections with `ROLLBACK`, which is what makes a
  transaction dropped without commit (the simulated crash) leave nothing behind instead of leaking
  into the next caller.
- The merge-to-`main` `integration` job now runs `store-postgres` against a real `postgres:16`
  (pinned by digest) — the twelve `EventStore` contract cases plus cloud-only tests for RLS isolation
  and monthly partition routing. These live behind the crate's `integration` Cargo feature, so the
  ten-minute pull-request gate neither compiles nor runs them and stays database-free; `just
  test-integration` runs them locally against your own PostgreSQL. `deny.toml` gains three reasoned,
  dated skips for the transient version duplications the `tokio-postgres` stack brings in (the rand
  0.10 line for its SCRAM nonce, and `fallible-iterator` mid-migration).
- ADR-0031 — cloud adapter transports: `async-nats` for `link-nats` (the JetStream client protocol is
  the "genuinely hard and general" infrastructure ADR-0007 says to buy); hand-rolled SigV4+HTTP for
  `blob-garage` (thin and scheduled for deletion once WAL shipping is in-house, so no S3 SDK); a
  bounded-queue HTTP importer for `metrics-vm` (off the sales path, so `record` never blocks).
  Registered in the ADR index and the engineering guide.
- `metrics-vm` (P7): the cloud `MetricsSink` over `VictoriaMetrics`. `record` enqueues into a bounded
  in-memory queue and returns without waiting; a background task flushes batches through a
  transport, so a slow or dead metrics backend drops samples rather than blocking a sale (ADR-0026
  contract 1). No floating point — a sample is an `i64` and a unit, and the unit rides across as a
  label. The transport is hand-rolled HTTP/1.1 over `tokio` to `VictoriaMetrics`' JSON line import
  (`/api/v1/import`), no client crate (ADR-0031). Because the port's contract is this adapter's
  queueing rather than `VictoriaMetrics`' storage, its shared contract suite runs **in process**
  against a capturing transport in the ordinary `test` job, and a separate in-process HTTP mock pins
  the exact import bytes — no live `VictoriaMetrics` needed to verify it. Adds no new external
  dependencies (`tokio` and `serde_json` were already in the tree).
- `blob-garage` (P7): the cloud `BlobStore` over Garage / S3. Thin and temporary by design — object
  storage exists only for Litestream and the port is deleted once WAL shipping is in-house (ADR-0007)
  — so rather than an S3 SDK it hand-rolls SigV4 signing and HTTP/1.1 over `tokio` (ADR-0031),
  path-style, plain `http://`. `put`/`get`/`delete` are idempotent (a repeated put overwrites, an
  absent get is `Ok(None)`, a repeated delete succeeds), and `list` is segment-aware: S3's `prefix`
  is a string match that also returns `stores/10` for `stores/1`, so the adapter filters the result
  through `BlobKey::is_under`, which is what keeps one tenant's listing out of another's. Verified
  three ways: the SigV4 signer's arithmetic against AWS's published `get-vanilla` vector (no server),
  the full contract suite against an in-process S3 mock (request/response framing and the prefix
  filter, in the ordinary `test` job), and — behind the `integration` feature — the same suite
  against a real MinIO in the merge-to-`main` job. Signing uses `hmac`/`sha2` pinned to the
  RustCrypto line already in the tree, so no new duplicate version is introduced.
- `link-nats` (P7): the store→cloud `MessageLink` over NATS JetStream, on `async-nats` — the one
  cloud adapter that carries a real client dependency, because the JetStream protocol is the hard,
  general infrastructure ADR-0007 says to buy (ADR-0031). Outbound only and at-least-once: no
  transaction across NATS and the edge database, so the outbox makes a crash between commit and
  publish safe. The handshake is local — reachability, stream existence, and `pos_proto`'s
  `negotiate` — so the link stays one-directional with no cloud responder. Back-pressure is visible:
  the stream is `discard: new`, a full stream returns `resource_exhausted` (retryable, so the outbox
  holds), and `capacity` reports the fill level the 80% alert reads. Verified against a **real NATS
  server with JetStream** (behind the `integration` feature, wired into the merge-to-`main` job as a
  `docker run -js` step) — all six `MessageLink` contract cases including the severed-link and
  full-stream obligations. `async-nats` is pinned to 0.50 (its `rustls-webpki` is on the patched
  0.103 line; 0.38's 0.102 carried fresh RUSTSEC advisories); its `webpki-roots` (Mozilla CA bundle,
  `CDLA-Permissive-2.0`) is a scoped, reviewed `deny.toml` licence exception, and a
  `skip-tree = async-nats` collapses the transient version straddles its stack introduces.
- `pos-cloud` (P7, first slice): the cloud binary, and its ingest→rollup spine. `Cloud::ingest`
  stores a batch idempotently in one transaction — a replay adds `duplicates`, not `appended`, and
  grows the log by nothing (ADR-0026 §4) — and `Cloud::daily_rollups` folds the log into per-store,
  per-trading-day activity counts (the read model dashboards will answer from, `docs/roadmap.md`
  P7). Both are generic over the `EventStore`, so the same code runs against `pos-fakes` in tests
  and `store-postgres` in the cloud (ADR-0026); the spine is verified against the fake with no
  database. The binary loads config, opens and migrates the PostgreSQL store, and serves an axum
  router (`/health` and `/internal/ingest`, the reconciliation re-push target). Deliberately later,
  each its own slice: the public `/v1` API and generated OpenAPI (ADR-0019), the NATS cursor
  consumer that drives ingest in production, webhooks, super-admin auth (Argon2 + TOTP), the
  four-level config tree, the retention/PII-masking cron, and the dashboard screens with
  materialised rollups.
- `pos-cloud` (P7): the public `/v1` read API and its **generated** OpenAPI (ADR-0019).
  `GET /v1/stores/{store_id}/rollups/daily` returns a store's per-trading-day activity rollups; a
  malformed store id is a `400`, an unreachable store a `503`. The OpenAPI document at
  `GET /v1/openapi.json` is generated from the handlers (`utoipa::path`) and their response types
  (`utoipa::ToSchema` beside the `serde` derives), never hand-written, and is committed at
  `docs/openapi.json`. A `pos-cloud` test renders that file and fails CI on any drift — the same
  opt-in idiom (`POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi`) as `pos-proto`'s
  event-catalogue snapshot, so the document can never disagree with the code. `/internal/*` stays out
  of the external contract by construction.
- `pos-cloud` (P7): the **NATS cursor** — the production ingest feed. `link-nats` gains a
  `NatsConsumer`, the read counterpart of `NatsLink`: a durable JetStream pull consumer whose delivery
  position lives server-side (the "cursor over the event log" a later slice resets to replay), which
  hands the caller each decoded batch *with* its message handles and acknowledges only after the
  caller has stored it — at-least-once with exactly-once effect, given idempotent ingest. A frame
  that is not a valid envelope can never be ingested, so it is terminated and counted (loudly, never
  silently) rather than wedging the cursor. `pos-cloud`'s `cursor` loop drives `Cloud::ingest` from it
  and applies the ack policy (advance on commit, redeliver otherwise, never drop); the policy is a
  pure function tested without a broker, and the whole path is proven against real JetStream by
  `link-nats`'s and `pos-cloud`'s `integration` suites. The binary starts the cursor when a `[nats]`
  config section is present and shuts it down with the HTTP server; absent that section it serves
  reconciliation re-pushes only. ADR-0031 is amended to record the consumer and its testing.
- `pos-cloud` (P7): the **webhook delivery engine** (ADR-0032) — how a tenant receives events as
  signed HTTPS `POST`s. A webhook is **a cursor over the event log, not a queue**: each endpoint
  stores only its position, so a dead endpoint falls behind without the cloud buffering anything (the
  P7 exit criterion), and a failed delivery simply does not advance the cursor. Four safety rails,
  each a separately-tested module: **HMAC-SHA256 signing** over a `"{timestamp}.{body}"` payload with
  a `v1=` header and a ±5-minute replay window (bound into the signature, so a capture cannot be
  re-stamped); **SSRF vetting** that requires https, forbids URL credentials, and refuses any host
  that resolves to a non-public-unicast address (loopback, RFC-1918, the `169.254.169.254` metadata
  range, CGNAT, ULA, IPv4-mapped v6, documentation, reserved, multicast), connecting only to the
  vetted addresses so DNS rebinding cannot slip through; a **circuit breaker** that backs a failing
  endpoint off and **auto-disables** it after 24 hours of continuous failure; and full **per-endpoint
  isolation** (one cursor, one breaker each). The whole engine — signing, replay window, SSRF
  classifier, breaker, and the falls-behind cursor — is unit-tested with no broker, network, or
  database, against explicit crypto and IP-range vectors. The concrete TLS sender (behind a
  `WebhookTransport` seam) and endpoint persistence are deliberately later slices (ADR-0032). Adds
  `url`, `hmac`, `sha2` — all already in the workspace tree, so no new dependency version enters.
- `pos-cloud` (P7): the **four-level configuration tree** (ADR-0033) the cloud owns and publishes,
  resolving `docs/roadmap.md`'s D10 open questions. A store's effective configuration is the deep
  merge of four authored layers — Tenant → Brand → Store → Device, most-specific winning, nested
  objects merged and scalars/arrays replaced. **Deltas are RFC 7386 JSON Merge Patch**: a present key
  overrides, a nested object recurses, and `null` deletes — so a delta can remove a key, and `diff`
  then `apply` round-trips (property-tested over many pairs). A candidate version is **validated in
  the cloud before it is published**, reusing `pos-core`'s §10 inter-flag capability rules
  (`capability::conflicts`) so the cloud cannot bless a flag combination the edge would reject; a
  rejected publish changes nothing, so the **last good version stays current**. A store reports the
  version it holds and gets a **delta when it is within K (default 20) versions of current**, or a
  **full snapshot** when it is further behind or holding a version the cloud no longer retains — the
  "more than K behind ⇒ snapshot" rule made concrete. The engine produces the `ConfigUpdate` values
  the `ConfigStore` port carries and is pure (no persistence, no I/O); its persistence and the admin
  routes are a later slice. `pos-cloud` now composes `pos-core` (pure) for the capability rules.
- `pos-cloud` (P7): **super-admin authentication** (ADR-0034) — the two-factor sign-in guarding the
  admin surface. The password is hashed with **Argon2id** (the same primitive and crate the edge uses
  for PIN hashes; only the PHC hash is stored, never the password), and a **mandatory RFC 6238 TOTP**
  second factor is required — there is no password-only path. TOTP runs over **HMAC-SHA256** (RFC 6238
  permits it, authenticators honour `algorithm=SHA256`), chosen so the cloud reuses the `sha2`/`hmac`
  already in its tree instead of adding a second SHA1 crate version; codes are 6-digit on a 30-second
  step, accepted within a ±1-step skew window, and **single-use** (verification returns the matched
  step and refuses any step at or below the last one used, blocking replay). Both factors are
  evaluated before any verdict and the specific failure is server-log-only, so a prober learns
  nothing about which factor was wrong. The session cookie is **host-only** — `__Host-` prefixed,
  `Secure; HttpOnly; SameSite=Strict; Path=/`, and deliberately **no `Domain`** — so an admin session
  never crosses to another tenant's subdomain (the roadmap's named worst-case isolation failure). The
  auth core is pure and unit-tested with no clock or network: against RFC 6238's SHA256 vectors,
  secret redaction from `Debug`, the mandatory-second-factor/no-oracle rules, replay refusal, and the
  cookie attributes. Adds `argon2` (already at the edge); no new crypto crate for TOTP. The login
  route, credential persistence, TOTP enrolment, and per-tenant API keys are later slices.
- `pos-cloud` (P7 / Track A6): the **retention + PII-masking cron** (ADR-0035) — the data-protection
  enforcement PDPD (Decree 13/2023), GDPR, and CCPA require. Personal data (a marketplace order's
  name/phone/address, a corporate invoice's buyer fields) lives in the `SubjectId`-keyed subject
  store, never in the event log; once a record is past its retention period the cron **masks** it —
  personal field values become `[REDACTED]` while the `subject_id` and timestamps survive, so
  invoices still reference a subject and the books still reconcile. Masking (not row-deletion) is
  chosen precisely to keep that reference; it is one-way and idempotent. The retention period is
  **configuration** (per-country default, ADR-0027), never a code guess. The daily sweep is bounded
  (paged, no whole-table load) and idempotent (only unmasked records are read), and a failed run is
  retried rather than crashing the cloud. Scope is deliberate: it enforces the *automatic, time-based*
  policy over customer/buyer data only — never employee data (there is no behaviour monitoring), and
  it is **not** the path for an individual's erasure/access/portability request, which stays escalated
  to the Data Protection contact. The engine is pure behind a `SubjectStore` seam and unit-tested with
  no database or clock (masking scrubs every value yet preserves the id, is idempotent, leaks no
  original value; the sweep masks exactly the records past retention), using only placeholder test
  data. No new dependencies. The subject-store schema and the runner's wiring into `main` are later
  slices (the corporate-invoice buyer fields land with P10).
- `pos-cloud` (P7): **dashboards answered from materialised rollups** (ADR-0036) — the P7 exit
  criterion (a dashboard answers in under 10 ms). `Cloud::daily_rollups` computes the rollup from the
  log every call (O(events)); the new `dashboard` module keeps the rollup **materialised**: a
  projector cursor folds each event exactly once (idempotent, incremental, rebuildable by resetting
  the cursor), and the dashboard read answers from that stored rollup — its signature takes no
  `EventStore`, so it is O(days) and *cannot* scan the log. Both paths fold with one shared
  `fold_event` (`daily_rollups` was refactored onto it, behaviour unchanged), so the materialised
  rollup equals a full re-scan **by construction** — asserted by a test comparing the two, alongside
  idempotency, incremental-across-appends, and reads-only-the-rollup-store. Pure and I/O-free behind a
  `RollupStore` seam; no new dependencies. The `store-postgres` rollup table, the background projector
  task, and pointing `/v1/stores/{id}/rollups/daily` at the materialised read (same response shape,
  so no OpenAPI change) are the remaining wiring.
- `pos-cloud` (P7): **scoped per-tenant API keys** (ADR-0037) — the bearer credential machine
  integrators present to the public `/v1` API, and the isolation boundary for it. A key is
  `pos_<id>_<secret>`; the cloud stores only `SHA-256(secret)` (a fast hash is correct for a
  high-entropy random token — Argon2 is for guessable human passwords, ADR-0034), verifies in
  constant time, and every key is **bound to one tenant** (`Grant::tenant`, checked against the
  resource) and **deny-by-default by scope** (`Grant::authorizes`), so a key reaches one tenant's data
  and only the capabilities it was granted. Keys are revocable, optionally expiring, and the full
  token is shown once (only the hash is kept); the rejection reason is server-log-only, so it is not
  an enumeration oracle. The `pos_` prefix is fixed so a leaked key trips secret scanners. Lives
  beside the super-admin auth in `auth::apikey`; pure and unit-tested (issue→present→verify
  round-trip, wrong secret, id mismatch, revoked, inclusive expiry, malformed tokens, deny-by-default
  scoping, and no secret leaking through `Debug`), with obviously-fake test secrets. Reuses `sha2`; no
  new dependencies. Key persistence, the provisioning route, and the `/v1` bearer extractor are the
  remaining wiring.
- `pos-cloud` (P7): the **`/v1` bearer authentication seam** (ADR-0037) — `auth::bearer`, the HTTP
  edge over the pure API-key engine. `authenticate` reads the `Authorization: Bearer pos_…` header,
  looks the key up by its public id through a new `ApiKeyStore` lookup seam, and verifies it against
  the clock; `require_scope` then gates the specific action. Two rules are enforced structurally: a
  **no-oracle refusal** — a missing/malformed header, an unknown id, a wrong secret, a revoked or
  expired key all render one indistinguishable `401` (the reason is server-log-only), so a prober
  cannot enumerate keys — and a **store outage answers retryably** (`503`), never as a false denial
  that would make a caller discard a good key. A missing scope is a separate `403`, safe to
  distinguish because identity is already proven. Unit-tested with an in-memory key store and a fake
  clock (valid key → grant, unknown id and bad secret indistinguishable, missing/wrong-scheme header,
  store-outage `503`, ungranted-scope `403`, and all three credential problems rendering the identical
  `401`).
- `pos-cloud` (P7): the materialised-rollup read seam is now **keyed by `(tenant, store)`**
  (ADR-0036). `RollupStore::load`/`save`, `dashboard`, and `project` all take a `TenantId`; because a
  `/v1` caller's tenant comes from its authenticated `Grant` and never from the request, a caller can
  only read rollups for a store within its own tenant, and guessing another tenant's `store_id` reads
  back empty rather than that tenant's data — tenant isolation is a fact of the key, not of a check a
  handler might forget. Library-only refinement (no route or binary yet on it); the from-log
  `Cloud::daily_rollups` reconciliation path is unchanged.
- `store-postgres` (P7): the **deferred cloud persistence** now has its tables and query types.
  Migration `0002` adds `rollups` — one `jsonb` row per `(tenant_id, store_id)` holding the whole
  materialised `StoredRollups` (cursor + per-day counts), so a dashboard read is one primary-key
  lookup, not a log scan (ADR-0036) — and `api_keys` — `id` (the public ULID), `tenant_id`,
  `secret_hash` (bytea, SHA-256), `scopes` (text array of wire names), `revoked`, `expires_at`
  (epoch milliseconds), looked up by primary key (ADR-0037). Both are additive and idempotent; the
  rollup table carries RLS on `tenant_id` like the event log. New `PostgresRollups` and
  `PostgresApiKeys` handles (built from `PostgresStore::rollups()` / `.api_keys()`, sharing the pool)
  hold the SQL and return plain rows; `pos-cloud` implements its `RollupStore` and `ApiKeyStore`
  seams over them in a `persistence` module that does the domain conversion — all SQL in the adapter,
  all conversion in the cloud, no cloud type crossing into the adapter. `Scope` gained wire-name
  mapping (`read_rollups`/`read_events`/`manage_webhooks`, unknown names dropped deny-by-default) and
  `StoredApiKey::from_parts` rehydrates a key from a row.
- `store-postgres` (P7): a **cross-tenant isolation test for the rollup table** on real PostgreSQL —
  tenant A's `save_state` is invisible to tenant B naming the same `store_id` (the `(tenant, store)`
  key is the boundary the `/v1` dashboard rests on), and an `app_tenant`-role session scoped to one
  tenant sees only that tenant's rollup rows via RLS. Alongside the existing event-log RLS cases,
  this closes the "cross-tenant isolation proven by tests" half of the P7 exit criterion. Gated
  behind the `integration` feature; runs in the merge-to-`main` job against `postgres:16`.
- `pos-cloud` (P7): the persistence and the auth are now **wired into the running binary**, so `/v1`
  is a real authenticated, tenant-isolated, materialised-read surface. The router carries one
  `CloudApp` state bundling the event store, the rollup read model, the API-key store, and a
  `SystemClock`; `GET /v1/stores/{store_id}/rollups/daily` now **requires a bearer API key** with the
  `read_rollups` scope (missing/invalid → one indistinguishable `401`, wrong scope → `403`), and
  answers from the **materialised rollup** for the **key's own tenant** — the tenant comes from the
  verified grant, never the request, so a caller reading another tenant's `store_id` gets an empty
  list, never that tenant's data. `main` opens one Postgres pool and takes three views of it (event
  store, rollups, API keys). The internal `/internal/ingest` and `/health` stay unauthenticated
  (private network only). The generated OpenAPI now declares the `api_key` bearer scheme and the
  route's `401`/`403` responses and security requirement (snapshot regenerated). Router tests cover
  the authorised read, a missing key (`401` + `WWW-Authenticate`), a wrong-scope key (`403`), a
  foreign-tenant key reading empty, and a malformed store id (`400`), all against the fakes.
- `pos-cloud` (P7): the **rollup projector** background task, now the single writer of the
  materialised rollup the `/v1` dashboard reads (ADR-0036). Ingest only appends to the event log;
  each interval (configurable, default 30 s) the projector lists the fleet — a new `StoreCatalog`
  seam, answered in `store-postgres` from the distinct `(tenant_id, store_id)` of the log — and
  folds every store's new events into its rollup via the existing `project`, advancing the per-store
  cursor so each event is folded exactly once. Robust: one store's projection failing is logged and
  counted, not fatal; only a failure to list the fleet ends a tick, and the next retries. Wired into
  `main` alongside the ingest cursor and shut down with it on SIGINT. Without it the wired `/v1` read
  would return empty in production, so this is what makes the dashboard slice live. Unit-tested
  against the fakes (a pass folds the fleet then is idempotent; an empty fleet does nothing).
- `pos-cloud` (P7): the **super-admin login is wired** (ADR-0034), turning the pure two-factor check
  into a working `/admin` surface. A new `auth::admin` seam (`AdminStore`) loads the single credential
  and its last-used TOTP step and backs a server-side session table; `POST /admin/login` runs the
  password + mandatory-TOTP check and, on success, mints a **256-bit CSPRNG** session token (via the
  `getrandom` the edge already uses), stores only its `SHA-256`, and sets the host-only `__Host-`
  session cookie. Every credential problem — wrong password, wrong or replayed code, unprovisioned
  admin — collapses to one generic `401` (no oracle), a store outage is a retryable `503`, and the
  matched TOTP step is burned before the session is written so a code cannot mint two sessions.
  `POST /admin/logout` revokes the session and clears the cookie (idempotent); `GET /admin/session` is
  the guard the rest of `/admin` will stand behind. Persistence is `store-postgres` migration `0003`
  (a single-row `super_admin` table and an `admin_sessions` table, neither tenant-scoped — the
  super-admin is global — so neither carries RLS or an `app_tenant` grant); the session TTL is
  configuration (`admin_session_ttl_secs`, default eight hours). `CloudApp` now carries the admin
  store as a fifth collaborator. Adds no crypto crate (`getrandom` was already in the tree). Unit
  tests cover the no-oracle rule, replay refusal, expiry and logout against the fakes; router tests
  cover login→cookie→guard, a cookieless `401`, a wrong password, and logout; and a `store-postgres`
  integration test proves the credential round-trip, the monotonic step advance, and the session
  lifecycle against a real database.
- `pos-cloud` (P7): the **API-key provisioning surface** is wired (ADR-0037), completing the machine
  side of `/v1` auth. Behind the super-admin session guard: `POST /admin/api-keys` mints a CSPRNG id
  (a ULID) and a 256-bit secret at the edge, `issue`s the key, persists only the secret's hash
  (`store-postgres`, migration `0002` table), and returns the full `pos_<id>_<secret>` token **once**
  in the `201` body — it is never recoverable after; `GET /admin/api-keys?tenant_id=…` lists a
  tenant's keys as metadata only (id, scopes, revoked, expiry — never a secret or its hash); and
  `DELETE /admin/api-keys/{id}` revokes idempotently. An unknown scope name on provisioning is a
  `400`, not the silent drop the deny-by-default *read* path applies, so a typo cannot issue a key
  that grants nothing. A new `ApiKeyAdminStore` seam (insert / list / revoke) sits beside the
  read-only `ApiKeyStore`, so the per-request bearer path stays minimal. Adds no dependency. Router
  tests prove a provisioned token then authenticates a real `/v1` read and stops the moment it is
  revoked, that provisioning is closed without a session (`401`), and that an unknown scope is
  refused (`400`); the `store-postgres` integration suite covers insert → list → revoke.
- `pos-cloud` / `store-postgres` (P7): the **config tree now persists** (ADR-0033). A new
  `ConfigTreeStore` seam and the `store-postgres` `config_trees` table (migration `0004`) round-trip a
  store's whole tree — its four authored layers and its published version history — as one `jsonb`
  document per `(tenant, store)`, keyed and RLS-isolated by tenant exactly as the rollup read model.
  The pure engine gains `ConfigTree::state` / `ConfigTree::from_state` to export and rehydrate that
  state: the layers and history come back exactly as stored (so the current version and effective
  document are unchanged across a restart, and the last good version stays current), the validator is
  supplied fresh on load (behaviour, not state), and the history is trusted as already-validated
  rather than re-published. Adds no dependency. Unit-tested on the engine (serialise → rebuild →
  same effective document and same delta/snapshot decision) and against a real database in the
  adapter's integration suite (save → load → upsert → tenant-scoped miss). The admin authoring
  routes and the publish path to a store remain the next slice.
- `pos-cloud` (P7): the **config-tree admin routes** are wired (ADR-0033), behind the super-admin
  session guard. `PUT /admin/stores/{store_id}/config/{level}` (level ∈ tenant/brand/store/device)
  loads the store's tree for the query's tenant, replaces that level's document with the request
  body, and publishes — composing the four layers, validating (including pos-core's §10 inter-flag
  capability rules), and, only if valid, appending a version (a ULID minted at the edge) that is then
  persisted; the `200` carries the new `config_version_id`. An incoherent version — e.g.
  `pay_first_enabled` with `tables_enabled` — is a `422` carrying the violations, and nothing is
  stored, so the last good version stays current. `GET /admin/stores/{store_id}/config` returns the
  current effective (deep-merged, most-specific-wins) document, or `404` if the store has none yet.
  The tenant is named on the query string (the super-admin is global). `CloudApp` gains the
  config-tree store as a sixth collaborator. Router tests cover publish → override → effective-merge,
  the incoherent-config `422`, the cookieless `401`, and the unpublished-store `404`. The publish
  path that delivers a `ConfigUpdate` to a store over the wire remains the next slice.
- `pos-cloud` / `store-postgres` (P7 / Track A6): the **retention / PII-masking cron is wired**
  (ADR-0035). `store-postgres` migration `0005` adds the `subjects` table — the one place personal
  data lives, keyed by a globally-unique `subject_id`, with `collected_at`/`masked_at` as epoch-ms and
  `fields` as jsonb, RLS-isolated by tenant and carrying a partial index for the sweep's "unmasked,
  past cutoff" query — and `PostgresSubjects` implements the `SubjectStore` seam. Masking overwrites
  the field values in the row (the PII is gone from the database, not flagged), and the
  `masked_at IS NULL` write guard makes it idempotent at the database. `main` starts the daily runner
  **only when `retention_days` is configured**: the period is a legal decision, not a code default, so
  with none set the cron stays off (masking on a guessed schedule would erase early or keep too long,
  both violations); the sweep interval defaults to daily. The cron masks (never deletes) so the books
  stay reconcilable, touches only customer/buyer data (never employee data — there is no
  employee-behaviour monitoring), and is not the path for an individual's erasure/access request —
  those stay escalated to the Data Protection contact. Adds no dependency. Proven by a `store-postgres`
  integration test (fetch-due → mask → not-re-fetched → not-re-masked) and a `pos-cloud` runner test
  (one sweep, then clean shutdown). The writer that populates the subject store lands with P10/P11.
- `pos-cloud` / `store-postgres` (P7): **webhook endpoints now persist** (ADR-0032). A webhook is a
  cursor over the event log, so a subscription is only its durable facts — destination, signing
  secret, cursor, disabled flag — never a backlog. `store-postgres` migration `0006` adds the
  `webhook_endpoints` table (`id` ULID PK, `tenant_id`, `store_id`, `url`, `secret`, `cursor` NULL
  until first delivery, `disabled`), RLS-isolated by tenant, with a `tenant_id` index for the admin
  listing and a partial index (`WHERE disabled = false`) for the delivery task's enabled-load. Unlike
  an API-key secret (stored as a hash), the signing secret is kept **in full** because the cloud
  *signs* every delivery with it, so it must be recoverable; `SigningSecret` gains an `expose_secret`
  accessor for the persistence layer alone. `pos-cloud` fills its new `webhook::store::WebhookEndpointStore`
  seam over `PostgresWebhooks`: the tenant-scoped listing never carries the secret, while the delivery
  task loads the enabled fleet **fleet-wide as the trusted role** (RLS bypassed), the same posture as
  the rollup projector and the retention sweep. Adds no dependency. Proven by a `store-postgres`
  integration test (register → tenant-scoped list → fleet-wide enabled load → advance cursor →
  auto-disable suppresses → scoped delete). The admin CRUD routes and the concrete TLS sender remain
  their own later slices (ADR-0032).
- `pos-cloud` (P7): the **webhook admin routes** are wired (ADR-0032), behind the super-admin session
  guard. `POST /admin/webhooks` SSRF-vets the destination first — `https` only, no credentials, and
  every resolved address must be public unicast — running `vet` with a real `getaddrinfo` resolver on
  the blocking pool, then mints a CSPRNG id and signing secret, persists the endpoint, and returns the
  signing secret **once** (the tenant's copy of what the cloud signs deliveries with). A loopback,
  link-local (the `169.254.169.254` metadata range), private, or plaintext URL is a `400` before
  anything is stored. `GET /admin/webhooks?tenant_id=…` lists a tenant's endpoints as metadata only
  (never the secret) and `DELETE /admin/webhooks/{id}?tenant_id=…` removes one within its tenant,
  returning `204` either way — deletion is idempotent and the tenant scope stops one tenant deleting
  another's. `CloudApp` gains the webhook-endpoint store as a seventh collaborator. Router tests cover
  register → list → delete and the SSRF/plaintext refusals over the fakes, using IP-literal
  destinations so vetting needs no DNS. The concrete TLS sender and the dispatch background task remain
  the next slice.
- `pos-cloud` (P7): the **concrete webhook TLS sender** (ADR-0038, a new ADR) — `TlsWebhookSender`, the
  `WebhookTransport` that turns a signed body into one HTTPS `POST`. It is built on the rustls/hyper
  stack **already in the tree** (`hyper`/`hyper-util` via axum; `tokio-rustls`/`webpki-roots`/`ring`
  via async-nats), so it adds direct-dependency lines — `hyper`, `hyper-util`, `http-body-util`,
  `bytes`, `tokio-rustls`, `webpki-roots` — not a new subtree, and no new `cargo-deny` entry. The
  sender **owns its dial**: it opens a TCP connection to one of the endpoint's *pre-vetted* addresses
  and performs the TLS handshake against the URL's hostname, never re-resolving — so it closes the
  DNS-rebinding gap between the SSRF check and the connect by construction. The `ring` crypto provider
  is selected explicitly (and `tokio-rustls` pinned with `default-features = false`) so `aws-lc-rs`
  cannot enter through feature unification; roots are the bundled Mozilla set (hermetic, no
  base-image `ca-certificates` dependency). Each delivery is bounded by a timeout — a black-hole
  endpoint cannot wedge the dispatch loop; a timeout is an ordinary failed delivery. The pure
  request-derivation (host/SNI, origin-form target, `ip:port` dial set, the two signature headers) is
  unit-tested without a network; the handshake belongs to the gated integration lane and the soak. The
  dispatch background task that drives this across the enabled fleet remains the next slice.
- `pos-cloud` (P7): the **webhook dispatch background task** is wired into `main` (ADR-0032), closing
  the webhook feature end to end. Each tick it loads the enabled endpoints fleet-wide (as the trusted
  role, like the projector and the retention sweep), **re-vets** each URL so it can only connect to a
  currently-approved address (closing the DNS-rebinding gap before every delivery batch, not just at
  registration), and delivers the events after each cursor over TLS with the `TlsWebhookSender`,
  persisting each cursor advance so a restart resumes where it left off and persisting a 24-hour
  auto-disable so a dead endpoint drops out of the fleet. The live endpoints — with their cursors and
  **breakers** — are held in memory across ticks (breaker windows accumulate rather than resetting);
  the database holds only the durable facts. It always runs (a cheap no-op with no endpoints) and is
  bounded per endpoint per tick so one far-behind subscriber cannot starve the fleet. Two config knobs,
  both with defaults: `webhook_dispatch_interval_secs` (10s) and `webhook_delivery_timeout_secs` (10s,
  so a black-hole endpoint cannot wedge the loop). The sweep logic is unit-tested over the fakes
  (deliver → persist cursor → idle; a now-unsafe URL is skipped, not delivered to).
- `pos-cloud` (P7): the **config publish-to-store path** (ADR-0039, a new ADR) — the cloud now
  delivers a store its configuration, the half ADR-0033 had deferred. Because the store→cloud link is
  outbound-only (ADR-0031, no cloud→store push channel exists), delivery is a store-initiated **pull**
  on a new store-facing surface: `GET /sync/stores/{store_id}/config?held_version=…` runs the config
  engine's `update_for` and returns `{"status":"up_to_date"}` or `{"status":"update","update":{…}}`
  carrying the RFC-7386 delta (or a full snapshot past K versions behind). It is authenticated by an
  API key with a new deny-by-default `read_config` scope and answers **only for the key's own tenant**
  — the tenant comes from the verified grant, never the path — so a store reaches only its own trees;
  an unknown or unpublished store reads `404`. `/sync` is a fifth route family (store operation, not
  the public integrator API), so it is absent from the OpenAPI document, like `/admin` and
  `/internal`. Reuses the existing API-key bearer and config-tree collaborators — no new dependency or
  `CloudApp` generic. Router tests cover snapshot → up-to-date and the `401`/`403`/`404` closes. The
  `pos_edge` loop that polls this and applies through `ConfigStore` is store-side fleet wiring (P9).
- `pos-cloud` (P7): **reset-cursor-and-replay** for the materialised rollup. `POST
  /admin/stores/{store_id}/rollups/reset?tenant_id=…` (behind the super-admin session guard) saves the
  store's rollup back to the empty default, clearing its per-store projector cursor; the next
  projector pass then re-folds every event from the start of the durable log, so the cloud's read
  model can be rebuilt without touching the event log (`docs/roadmap.md` P7, ADR-0036). `204`
  regardless — a store with no rollup yet resets to the same empty state. Reuses the existing
  `RollupStore` collaborator; a router test proves a seeded cursor is cleared and the cookieless call
  is `401`.
- `pos-cloud` / `store-postgres` (P7): **nightly reconciliation** (ADR-0040, a new ADR) — the cloud's
  emit-missing-ids side. Because ULIDs are not a dense sequence, the cloud cannot know what it dropped
  on its own, so reconciliation is an edge-initiated diff: `POST /internal/reconcile` takes
  `{tenant_id, store_id, event_ids:[…]}` and returns `{missing:[…]}` — exactly the ids the event log
  lacks, which the edge then re-pushes through the idempotent `/internal/ingest`. It lives on the
  private-network `/internal` surface beside ingest (unauthenticated, absent from the OpenAPI). A new
  `ReconcileStore` seam is answered by a `store-postgres` `event_id = ANY(candidates)` membership query
  scoped by tenant and store, bridged through `persistence.rs`; the endpoint is a small
  independently-stated sub-router merged into the main router in `main`, so reconciliation adds **no
  eighth `CloudApp` generic**. Proven by a router test over a fake (missing = candidates − present;
  a non-ULID id is a `400`) and a gated `store-postgres` integration test of the membership query
  (tenant/store-scoped, empty-set short-circuit). The `pos_edge` job that assembles the nightly
  manifest and re-pushes is store-side fleet wiring (P9).
- `pos-cloud` / `store-postgres` (P7): the **printer/KDS discover → propose → admin-approves** flow
  (ADR-0041, a new ADR). A store discovers a device on its LAN and proposes it; a super-admin approves
  it before it is usable — the human gate that keeps an unauthenticated port-9100 device off the
  fleet. `store-postgres` migration `0007` adds `device_proposals` (id, tenant_id, store_id, kind,
  name, address, status, timestamps), RLS-isolated by tenant with a partial index on the pending
  queue, behind a new `DeviceProposalStore` seam and `persistence.rs` bridge. `POST
  /sync/stores/{id}/devices` proposes (store-facing, API key with a new `manage_devices` scope, stored
  `pending`); `GET /sync/stores/{id}/devices` returns the store's **approved** devices — what the edge
  acts on, never a raw discovery. `GET /admin/devices/proposals?tenant_id=…` is the super-admin pending
  queue and `POST …/{id}/approve` / `…/{id}/reject` resolve one (idempotent: only a pending row
  transitions; tenant-scoped so one tenant cannot resolve another's). The routes are a merged
  sub-router carrying their own state, so device onboarding adds **no eighth `CloudApp` generic**
  (same shape as reconciliation). Proven over the fakes end to end (propose → pending queue →
  approve → approved list; scope/`session` closes) and by a gated `store-postgres` integration test
  (propose → list by status → one-way resolve → cross-tenant no-op). The `pos_edge` mDNS discovery
  loop is store-side wiring (P5/P9).
- `pos-cloud` (P7): the **menu-image pipeline** (ADR-0042, a new ADR). `images::render` turns a
  tenant-uploaded image into two JPEG renditions under hard byte budgets — a **≤30 KB thumbnail** and
  a **≤150 KB detail**. Because JPEG size is not a closed form of dimension and quality, each rendition
  walks a descending `(max_edge, quality)` ladder and takes the first attempt at or under budget; the
  ladders end aggressively enough that any real image fits, and an image that somehow does not is a
  `Budget` error rather than an over-budget object. It buys the `image` crate for the codec/resize work
  (ADR-0007: the genuinely-hard-and-general thing to buy), with **minimal features** (`png`+`jpeg`
  only) so `cargo-deny` passes with no new entry. The transform is pure `bytes → bytes` — no I/O — and
  unit-tested (a synthetic image yields two in-budget renditions that both decode; the thumbnail fits
  its pixel bound; a non-image upload is a clean `Decode` error). Where renditions are stored (a
  Postgres `bytea` table rather than the deletion-scheduled `blob-garage` port) and the admin upload
  route are the next slice.
- `pos-cloud` / `store-postgres` (P7): the **translation grid** (ADR-0043, a new ADR) — the cloud-side
  place a tenant authors its localized menu/content strings, feeding the edge's ICU i18n runtime
  (ADR-0020). The grid is `key → { locale → string }`, one `jsonb` per tenant (`store-postgres`
  migration `0008`, RLS-isolated), behind a new `TranslationStore` seam and `persistence.rs` bridge.
  The one structural rule is the always-present fallback: `PUT /admin/translations?tenant_id=…`
  (super-admin session) is rejected `422` naming every key that lacks a non-empty `en`, and nothing is
  stored — so the edge can always degrade to English rather than to a raw key; `GET
  /admin/translations?tenant_id=…` returns the grid (empty if none yet). Authoring is a validated
  whole-grid replace, in a merged sub-router carrying its own state (no `CloudApp` generic). Proven by
  a pure `keys_missing_fallback` unit test and a router round-trip (`PUT` → `GET`, the `422` on a grid
  missing `en`, and the `401` without a session). The store-facing `/sync` fetch that hands the edge
  its grid is the next slice, the same split config delivery drew.
- `docs/roadmap.md` (P7): records the cloud phase as **substantially complete** — every P7 cloud
  feature has landed (adapters; super-admin auth; API keys; the config tree with delta/snapshot
  publishing and store-facing pull; idempotent ingest → rollups + projector; the generated `/v1`
  OpenAPI; the full webhook stack; the NATS cursor feed; reconciliation; reset-cursor-and-replay; the
  image pipeline; the translation grid; the retention cron; device onboarding) and every P7 exit
  criterion is met by a test. Records the deliberate deferrals with their real homes: the store-side
  halves of the cloud↔edge loops (edge pollers) are P9 fleet wiring, and the `metrics-vm`
  sparse-sampling profile waits for metrics integration at scale (the cloud emits no metrics yet, so a
  second sink would have no producer; its stride is sized against P12's measured capacity).
- `deploy/` (P8, ADR-0044 — a new ADR): the **fork-and-deploy stack** for one country cell on a single
  VPS. `deploy/Dockerfile` builds `pos_cloud` multi-stage (a Rust 1.94.1 builder → a `debian:bookworm-slim`
  runtime carrying only the stripped binary and CA roots, running as a non-root uid `10001`).
  `deploy/compose.yml` runs the four P7 backends (`pos_cloud`, `postgres:16.4`, `nats:2.10` with
  JetStream, `dxflrs/garage`) behind a Caddy TLS ingress — every image pinned to an immutable tag, each
  service with log-size/file and ~memory/CPU/pids caps (~1.4 GB across the four), on an `internal`
  backend network so only Caddy faces the internet. `deploy/Caddyfile` + `deploy/caddy.Dockerfile`
  (Caddy xcaddy-built with the Cloudflare DNS provider) terminate real TLS: DNS-01 by default (grey-cloud
  the record, no inbound `:80` to ACME), with a documented `<vps-ip>.sslip.io` HTTP-01 fallback for a box
  with no purchased domain; Cloudflare "Flexible" SSL is forbidden. Operational secrets are **not** in the
  repo — `deploy/compose.yml` reads `deploy/secrets/{pos.env,cloud.toml,garage.toml,caddy.env}`, all
  git-ignored and generated on the box by `bootstrap.sh` (P8b, next). A root `.dockerignore` keeps the
  build context to the source cargo needs.
- `deploy/bootstrap.sh` (P8, ADR-0044): the idempotent server-side bootstrap. It mints the internal
  operational secrets **on the VPS** — the PostgreSQL password (`pos.env`), the `pos_cloud` config
  (`cloud.toml`, chowned to the app's uid `10001` so a `600` file stays readable by the non-root
  container), a NATS JetStream token (`nats.conf`, now enforced on the internal network), and a Garage
  `rpc_secret` (`garage.toml`) — writes them `600` under the git-ignored `deploy/secrets/`, and brings the
  stack up. Re-running keeps every existing secret rather than rotating it, so a second run cannot lock
  the box out of a database whose password was already deployed. Only the TLS values (`DOMAIN`,
  `ACME_EMAIL`, `CF_DNS_API_TOKEN`) come from outside, passed in the environment on the first run and
  written to `caddy.env`; an `*.sslip.io` `DOMAIN` needs no Cloudflare token (HTTP-01 fallback). It prints
  a **one-time super-admin setup token** once — the token that enrols the first super-admin through the
  first-boot provisioning route wired in P8c; `AdminStore` has no credential-writer yet, so that route,
  and the `reset_admin` break-glass beside it, are P8c's slice. No application code changed here.
- `pos-cloud` / `store-postgres` (P8, ADR-0045 — a new ADR): **first-boot super-admin enrolment**, the
  route that consumes the setup token above. `POST /admin/setup` provisions the single super-admin the
  first time — token-gated and self-disabling — closing the gap ADR-0034 left ("first-boot seeding, P8
  bootstrap"): the login existed but nothing could write the first credential. It compares the
  configured `admin_setup_token` in constant time, then hashes the operator's chosen password with
  Argon2id under a fresh CSPRNG salt, generates a 256-bit TOTP secret, and returns the enrolment (the
  `otpauth://` URI and its base32 secret) exactly once. It answers `404` when no token is configured,
  `401` on a token mismatch, `422` below a 12-character password, and `409` once an admin exists —
  provisioning is `INSERT … ON CONFLICT DO NOTHING` on the single-row `super_admin` table, so the first
  enrolment wins and the token is thereafter inert. `AdminStore` gains a `provision_credential` writer
  (backed by `store-postgres`; **no new migration** — the table already had the shape from migration
  `0003`), and a new `crate::auth::enrol` hand-rolls RFC 4648 base32 and the `otpauth://` URI (no new
  dependency). Proven over the fakes: enrol → `201` with a well-formed enrolment and a credential now
  present, a second call `409`, and the `404` / `401` / `422` refusals each provisioning nothing. The
  reset break-glass is a DB one-shot in the deploy workflow (next), gated by a GitHub Environment, so
  no `reset_admin` flag ever rides in the app's own container environment.
- `deploy/` (P8, ADR-0044/ADR-0045): the **deploy workflow** and the reset break-glass.
  `.github/workflows/deploy.yml` (manual `workflow_dispatch`, in a `production` GitHub Environment)
  builds the `pos_cloud` and Caddy images in CI, ships both over the existing SSH channel
  (`docker save | ssh docker load` — no registry, so GitHub still holds no application secret), and
  runs `bootstrap.sh` on the box with the loaded image tags and `POS_BOOTSTRAP_NO_BUILD=1` so the box
  never rebuilds from source. A rollback is re-running at an older commit; the app container is
  stateless. `bootstrap.sh` now writes the setup token as `admin_setup_token` into `cloud.toml` (where
  `pos_cloud` reads it) and gained the `POS_BOOTSTRAP_NO_BUILD` path. `reset_admin=true` runs
  `deploy/reset-admin.sh` after bring-up — `DELETE FROM super_admin; DELETE FROM admin_sessions;`, the
  ADR-0045 break-glass — gated by the Environment's required reviewer, the second human a wipe needs.
  The fork's 4–6 secrets (`VPS_HOST` / `VPS_USER` / `VPS_SSH_KEY` / `VPS_KNOWN_HOSTS` / `DOMAIN` /
  `ACME_EMAIL` / `CF_DNS_API_TOKEN`) are documented in the workflow and `deploy/README.md`; the SSH
  host key is pinned (`VPS_KNOWN_HOSTS`), not trust-on-first-use.
- `deploy/` (P8, ADR-0046 — a new ADR): **cloud backups and the restore drill**. Postgres now archives
  WAL continuously (`archive_mode=on` to a `wal_archive` volume, a fail-closed `archive_command`);
  `deploy/backup.sh` streams a compressed `pg_dump` out of the container and ships it off-box with
  `rclone` when `RCLONE_REMOTE` is set; the deploy workflow takes a `--label pre-update` snapshot before
  each new image comes up. Four unequal backup classes — continuous WAL, the daily dump, a weekly Garage
  object sync, and the pre-update snapshot — price each by its real recovery value. `deploy/restore-drill.sh`
  is the proof a backup restores: it dumps the live database, restores it into a throwaway one, and
  reconciles every public table's row count against the source, exiting non-zero on any mismatch — a
  silently unrestorable backup fails loudly. `nightly.yml`'s `restore-drill` job now runs it for real
  against a service Postgres seeded with a synthetic dataset (it was a placeholder echo). The
  store-backup half of the drill is edge WAL shipping (P9, spike A4) and joins when that lands.
  `/backups/` is git-ignored — a dump holds T1/T2 data and must never be committed.
- `k8s/` + `docs/deploy-runbook.md` (P8, ADR-0044): the **optional Kubernetes lane** and the
  **fork-to-UI runbook**. `k8s/pos-cloud.yaml` mirrors the Compose stack as a starting skeleton — the
  four backends as Deployments with PVCs and the Recreate strategy, `pos_cloud` running as the
  non-root uid `10001` with a `/health` readiness probe, an Ingress for DNS-01 TLS — with every
  environment-specific line (`storageClassName`, `ingressClassName`, the issuer, the image) marked
  `# ADAPT`; `k8s/README.md` documents the create-secrets-in-cluster model and that Compose stays the
  supported default. `docs/deploy-runbook.md` is the ordered checklist a forker follows: the
  Cloudflare rules (grey-clouded record, a scoped DNS token, "Flexible" SSL forbidden), the 4–6 GitHub
  secrets, the `production` Environment with a required reviewer, running the deploy, and enrolling the
  first super-admin. `docs/roadmap.md` records P8 as **substantially done**, with the store-backup half
  of the restore drill (P9) and the human end-to-end fork test as the noted deferrals.
- `crates/adapters/updater-minisign` (P9, ADR-0047 — a new ADR): the **minisign update-verification
  adapter**, the concrete `Signer` the P2 port anticipated ("real verification is minisign over a vetted
  Ed25519 crate; it will pass the same suite"). `MinisignVerifier` verifies a release artifact's
  signature over `ed25519-dalek` and `blake2` (both pure-Rust, no C), parsing minisign's binary blob —
  `algorithm(2) ∥ key_id(8) ∥ ed25519_sig(64)` — and both the `Ed` (raw) and `ED` (BLAKE2b-512 prehash)
  algorithms. It is **verify-only**: there is no `sign` method and it never holds a private key
  (`docs/architecture.md` §4 keeps signing offline). It honours the port's three status distinctions
  exactly — a wrong key id is `invalid_argument` ("try the other baked-in key"), a bad signature for the
  right key is `permission_denied` (terminal, never auto-retried into an install), and malformed bytes
  are `invalid_argument` — and is **total over hostile input**: every parse is a checked `slice.get`,
  under the backbone's `panic`/`indexing_slicing`/`unwrap`/`expect` denials, because verification runs at
  startup on attacker-chosen bytes. It passes the existing `signer` contract suite (the harness signs
  with throwaway seed-derived keypairs — real signatures, never a production key), plus a legacy-`Ed`
  round trip. `ed25519-dalek`/`curve25519-dalek`/`blake2` enter the graph and `cargo deny` stays green.
  The production keypairs remain the human-only offline step (P0/P9); whether a valid key is still
  trusted stays a revocation-list question for the config tree, and how an update rolls out is ADR-0048
  (next).
- `pos-core` (P9, ADR-0048 — a new ADR): the **OTA rollout decision**. `pos_core::ota::decide_rollout`
  is the pure function that answers "should this device install this update *now*?", once the artifact
  is validly signed (P9a). It weighs the device's version / ring / canary-bucket / last-self-test
  against the cloud's published update by a fixed precedence that *is* the safety argument — roll back
  (a device that failed its self-test on the running version reverts, outranking even the kill switch),
  already-current, halt (kill switch), refuse (revoked signing key), ring gate, fleet canary gate,
  install. The rollout shape is **published data** — `min_ring` (Lab < Pilot < Fleet) plus a
  `fleet_rollout_percent` that a stable per-device canary bucket is measured against — which settles the
  docs' inconsistent ring count: adding a "25% ring" is setting a number, not shipping a release. The
  logic is pure and stateless (the device persists only its current version and last self-test), so the
  simulator (P12) can exhaust it, and the signing key id is the raw `[u8; 8]` minisign uses, keeping
  `pos-core` off `pos-ports` (the edge maps `KeyId` at the boundary). Proven by twelve tests binding each
  precedence rule and the canary ramp. The `.pre-update` DB copy and the act of installing or reverting
  are edge I/O (P9e); revocation-list delivery is the config tree (ADR-0033).
- `pos-core` (P9, ADR-0049 — a new ADR): the **single-active lease** and its invoice-range handoff.
  `pos_core::lease` decides who is the active machine and hands a replacement a fresh invoice-number
  range, as pure functions. `lease_standing(held, authoritative)` compares two `LeaseGeneration`s and
  nothing else — **no clock** — so a lease never expires while the store is offline: equal ⇒ `Active`,
  behind ⇒ `Superseded` (the old machine goes read-only), ahead ⇒ `Invalid` (a generation the store
  never issued is refused, not trusted). `issue_replacement` starts the replacement's `InvoiceRange`
  exactly where the previous one ended, so the two are **disjoint by construction** — even a window
  where a still-offline old machine keeps issuing invoices cannot mint a legal invoice number the new
  machine also mints. Pure `pos-core`, no `pos-ports`; the wire `LeaseToken` stays the credential and
  the generation is the order that decides supersession. The actual invoice-range allocation is a
  `Fiscalization` call at the cloud (P10/A2), honouring `Superseded` → read-only is the edge (P9e), and
  legal invoice numbering stays distinct from the per-store gapless receipt counter (ADR-0025). Proven
  by tests binding the generation verdict, its time-independence, and the disjoint-forward range
  invariant across a chain of swaps.
- `pos-core` (P9, ADR-0050 — a new ADR): the **activation-code exchange**, the pure half of turning a
  fresh box into a trading machine. `pos_core::activation::ActivationCode` is a short human-typed code —
  eleven Crockford base-32 payload symbols (55 bits) plus a checksum symbol, shown as `XXXX-XXXX-XXXX` —
  that `parse` normalises (case-, hyphen- and whitespace-insensitive, folding the ambiguous `I`/`L`/`O`
  glyphs) and checksums, so a mistyped code fails at the keyboard rather than after a round-trip; the
  checksum is a typo guard, not authentication. `redeem` is the single-use rule: it grants only a code
  the cloud still records as `Issued`, refusing `Redeemed` (a spent sheet) and `Revoked` (a cancelled
  one), and a grant obliges the caller to flip the record in the credential-minting transaction — the
  atomicity is the cloud's, the rule the domain's. `device_activation` states that a box holding its
  `SecretName::DeviceCredential` is activated. Pure `pos-core`, no `pos-ports`; the code's `Debug` is
  redacted since it is a bearer credential until redeemed. Deliberately elsewhere (P9e): the cloud
  issue/look-up/consume endpoint and the edge client that presents the code over `MessageLink`, stores
  the credential via `KeyVault`, and emits `device.activation.completed`.
- `pos-cloud` + `store-postgres` (P9, ADR-0051 — a new ADR): the **cloud activation exchange**, the
  I/O half of ADR-0050. A super-admin issues an activation code bound to a device slot
  (`POST /admin/activation-codes`, shown once) and can cancel a slot's pending code
  (`POST /admin/activation-codes/revoke`); a fresh box presents its code on the **unauthenticated**
  `POST /activate` — the code is the credential — and receives a long-lived `posdev_<id>_<secret>`
  device credential in exchange. Redeem-and-mint is one transactional adapter method
  (`consume_and_provision`): `UPDATE … WHERE status = 'issued' RETURNING <slot>` is the single-use
  guard, and the `device_credentials` row is inserted for that slot in the same transaction, so a code
  can never be spent without minting exactly one credential for its own slot (composed seam calls
  cannot share a transaction, ADR-0016). The credential is api-key-shaped: high-entropy, `SHA-256`
  stored (not Argon2, ADR-0037), shown once; the code is stored only as `SHA-256` of its canonical
  text. Spent, revoked, unknown, and raced codes all collapse to one generic `403` (no oracle); a
  malformed code is a plain `400`. New: `pos_core::activation::ActivationCode::from_entropy` (code
  generation stays at the I/O edge), the `ActivationCodeStore` seam, `store-postgres` migration
  `0009_cloud_activation.sql` (`activation_codes` + `device_credentials`, no RLS — the api-key
  PK-lookup pattern), and the `PostgresActivationCodes` adapter. The edge client and the
  device-credential verification path are P9e; `device.activation.completed` is emitted edge-side, not
  in the exchange transaction. Proven by a router test (single-use + no-oracle) and a `store-postgres`
  round-trip (issue → atomic redeem+mint → replay-refused → revoke).
- `pos-core` + `pos-cloud` (P9, ADR-0052 — a new ADR): the **OTA rollout is published as
  configuration**. Rather than a new push channel, the cloud publishes the rollout through the existing
  config tree (ADR-0033/ADR-0004) as two keys — `fleet_update` (`target_version`, `min_ring`,
  `rollout_percent`, `halted`, `signing_key_id`, `revoked_key_ids`) fleet-wide, and `device_ota`
  (`ring`, `canary_bucket`) per device — and each store pulls it like any other setting. The schema and
  its rules live in `pos-core`: `FleetUpdateConfig`/`DeviceOtaConfig` are typed views whose `validate`
  methods parse the keys into the `PublishedUpdate`, revoked-key list, and `(Ring, bucket)` that
  `decide_rollout` (ADR-0048) consumes, so the cloud validates on publish and the edge validates on
  receipt through the *same* code and cannot disagree. `pos-core::ota` also gains `Ring::from_wire`/
  `as_wire`, `ReleaseVersion::parse`, and `parse_signing_key_id`. The cloud's `CapabilityValidator` now
  rejects an incoherent `fleet_update`/`device_ota` on publish — reporting every violation at once (a
  bad version, ring, ramp percent, or key id) — while an absent key means simply "no rollout
  configured", never an error. No new dependency, no new port. The edge updater that reads these keys
  and drives `decide_rollout` (verify → self-test → rollback) is P9e-4; fetching the update artifact
  bytes rides the edge→cloud transport still to be decided.
- `pos-ports` + `pos-contract-tests` + `pos-fakes` (P9, ADR-0053 — a new ADR): a **seventeenth port,
  `CloudSync`** — the store's request/response channel to the cloud, distinct from the deliberately
  outbound-only `MessageLink` (so the store still sells offline). It carries the two calls that need an
  answer back: `activate(code) -> {device_id, credential}` (the first-boot activation exchange,
  ADR-0050) and `fetch_update(release) -> bytes` (the OTA artifact, ADR-0048, verified against the
  minisign `Signer` before it is trusted — a transport is not a trust boundary). It names no domain
  type — the code arrives as a `&str` and the credential comes back as a `Secret` — so `pos-core` stays
  a sibling; and it is compile-time selected, so it has no `Dyn` mirror. `PortName` gains `CloudSync`
  and the port count moves to seventeen (`docs/architecture.md` §5 is authoritative; ADR-0021 amended,
  its immutable body preserved). A `CloudSync` contract suite and `FakeCloudSync` land with it —
  carried by the `SUITES` table and the every-port / every-suite assertions — so the count is enforced,
  not just documented. **No new dependency**: the port names only `pos-proto` and its own
  `Secret`/`PortError`. The real HTTP adapter that speaks to the cloud, and the edge activation client
  and OTA updater that consume the port, are P9e-2b onward.
- `cloud-sync-http` (P9, ADR-0054 — a new ADR): the **edge→cloud HTTP client**, the concrete
  `CloudSync` adapter the store composes. `activate(code)` posts `{code}` to the cloud's `POST /activate`
  (ADR-0050) and reads `{device_id, credential}` back; `fetch_update(release)` posts `{release}` to
  `POST /internal/ota/artifact` and reads the signed artifact bytes back (verified by the `Signer`
  downstream, not here — a transport is not a trust boundary). The socket lives behind an
  `HttpTransport` seam; everything else — request-shaping and the status→`PortError` mapping (`400` →
  `invalid_argument`, `403` → `permission_denied` with no oracle, `404` → `not_found`, else →
  `unavailable`) — is pure, so the shared `CloudSync` contract suite runs in the pull-request gate
  against a stub transport while the real TLS path (`TlsHttpTransport`) belongs to the gated integration
  lane and the soak. **No new dependency and no new `cargo-deny` entry**: the client reuses the exact
  rustls/hyper stack the webhook sender pins (ADR-0038) at the versions already in the tree. The cloud's
  `/internal/ota/artifact` route is defined by ADR-0054 and served by its P9e-4 counterpart.
- The ADR index (`docs/adr/README.md`) is caught up: ADRs 0038–0054 were missing from the table and are
  now listed.
- `pos-edge` (P9, ADR-0050/ADR-0053): the **edge device-activation flow and boot gate**. A generic
  `activation_router` composes `CloudSync` (exchange the code) and `KeyVault` (store the credential)
  as a sub-router — the same shape the cloud's activation routes take, since both ports are
  compile-time-selected with no `Dyn` mirror and cannot ride the concrete `AppState`. `POST
  /api/activate` parses the code locally (ADR-0050 — locally checkable), refuses a box that already
  holds a credential as a `409` (never a re-exchange of a now-spent code), exchanges it, stores the
  credential under `SecretName::DeviceCredential`, and records `device.activation.completed` via a new
  `Edge::record_activation` (a system event: no employee at first boot, the box's new identity is both
  the reporting and the activated device). `GET /api/activation` and `boot_standing` report the
  standing straight from the vault, which is the source of truth for "activated". Proven against the
  in-memory fakes and a stub cloud (`crates/pos-edge/tests/activation.rs`): a valid code activates and
  broadcasts the completion, the standing flips, a second attempt is a conflict, a wrong code is a
  no-oracle `403`, and a malformed one is rejected locally with nothing stored. Composition into the
  shipped binary waits on the OS-keyring `KeyVault` adapter, deferred as a gated hardware/OS handoff
  (it needs a live credential store its contract suite cannot get in the pull-request gate).
- `pos-edge` (P9, ADR-0055 — a new ADR): the **edge OTA updater**, the on-box orchestration that
  carries an eligible update through. `OtaUpdater` composes `CloudSync` (fetch the artifact), the
  `Signer` port (verify), and a new `UpdateInstaller` seam (the real-machine steps). `run` is a fixed,
  safety-ordered sequence: a `Superseded`/`Invalid` lease (ADR-0049) reports `ReadOnly` and touches
  nothing; `decide_rollout` (ADR-0048) resolves roll-back / halt / refuse / skip / install; and on
  install the artifact is **verified before the disk is touched** — read the claimed key id, refuse a
  revoked one, select the matching baked-in key, check the signature — then `stage_backup` (the
  `.pre-update` copy) → `apply` → `self_test`, committing only on a pass and rolling back on a fail.
  The five `UpdateInstaller` methods are the gated hardware/OS steps (write the binary, reboot, run the
  smoke test), left to the shipped binary and a real box; the orchestration around them is proven
  against the in-memory fakes and a recording installer (`crates/pos-edge/tests/ota.rs`, nine cases),
  which assert both the outcome and that a bad signature or an untrusted key writes nothing, and a
  failed self-test rolls back rather than commits. Composition into the binary waits on that real
  installer and the minisign keypair, both gated (`docs/roadmap.md` P9).
- `pos-cloud` (P11, ADR-0056 — a new ADR): the **public order-intake surface**, `POST /v1/orders` over
  the `OrderIn` port — the shared path `docs/roadmap.md` P11 builds first because the marketplaces and
  QR ordering (ADR-0012) all reuse it. `OrderIn` is a driving port (ADR-0026 §5): the store's edge
  implements it, and this endpoint is a caller (in the binary, the cloud→store relay; in tests,
  `FakeIntake`). A new `Scope::PlaceOrders` gates it, a `StoreDirectory` seam binds the request's store
  to the key's tenant so a key can never place an order into another tenant's store (unknown and
  not-yours both a generic `404`, no oracle), and the full `OrderIn` contract is surfaced unchanged: a
  first accept is `201`, an idempotent repeat `200` with `created:false`, an unknown item `400`, a
  rate-limit `429`, and a stale quote is *reported* (`repriced`) rather than honoured. A dedicated
  `OrderRequest`/`OrderResponse` DTO owns the wire shape (`pos-proto` carries no `utoipa`), and a
  guest note rides in as a `GuestNote` that cannot reach the event log. Proven against `FakeIntake` and
  a fake directory (`crates/pos-cloud/tests/cloud.rs`): accept, idempotent repeat, unknown item,
  cross-tenant and unknown store, missing scope, no bearer, and a QR order that awaits staff
  confirmation with a repriced stale quote. Serving it in the binary and registering it in
  `docs/openapi.json` lands with the cloud→store relay (P11a-2) — a buildable follow-on, not an
  external blocker; P10 (`countries/vn`) stays blocked on A2, and the payment (A1) and Grab/ShopeeFood
  (A3) adapters stay blocked-external.
- `pos-cloud` (P11, ADR-0057 — a new ADR): the **QR ordering** security keystone and guardrail
  decision. A guest scanning a printed table code has no API key, so the `table_id` travels as an
  HMAC-signed token — `{tenant}.{store}.{table}.{hex_tag}`, on the same `hmac`/`sha2` line and
  constant-time-compare idiom the webhook signer uses (ADR-0038), no new dependency —
  `mint_table_token` (admin side, printed into the QR) and `verify_table_token` (returns the
  `TableRef` or refuses a forged/tampered/cross-store token). The four guardrails ADR-0012 requires
  are one pure, total `evaluate(QrFacts) -> QrDecision` in a fixed precedence: a forged token is
  `UntrustedTable` first, then an offline store (`StoreOffline` — "ask a member of staff"), then
  out-of-hours, then the per-table rate limit; otherwise `Accept` carrying the staff-confirmation
  default (on). Both halves are pure and exhaustively unit-tested (`crates/pos-cloud/src/qr.rs`): the
  token mint/verify (known-shape, tamper, wrong secret, cross-table replay, malformed) and every
  guardrail branch and its precedence. The endpoint that gathers the facts (store-online, business
  hours, rate limiter) and relays a verified QR order into the `OrderIn` intake (ADR-0056) lands with
  the P11a cloud→store relay (P11b-2) — a buildable follow-on, not an external blocker.
- `shipping-ahamove` (P11, ADR-0058 — a new ADR): the first **`ShippingDispatch`** adapter, one of the
  two couriers `architecture.md` §6.1 names. It maps the port's three operations — book, cancel, track
  — onto a REST courier API behind a `CourierTransport` seam, exactly the transport-seam split the
  webhook sender (ADR-0038) and the edge→cloud client (ADR-0054) use: the socket lives behind the seam
  (`TlsCourierTransport`, the tree's pinned rustls/hyper stack, one trusted configured host so no SSRF
  surface), and building the request, mapping the courier's status vocabulary to `ShipmentStatus`, and
  mapping HTTP status to `PortError` are all pure. The shared `ShippingDispatch` contract suite runs in
  the fast gate against a **stateful stub courier** with no socket, proving the port semantics that do
  not change when a field is renamed: idempotent booking (`shipment_id` as the courier's idempotency
  key — a timed-out retry books no second rider), a cancel of a completed job refused
  `failed_precondition` rather than a quiet success that promises a refund, an already-cancelled job's
  cancel succeeding, an unknown job `not_found`, and a finished job still trackable so a missed
  callback reconciles. An unmapped courier status stays an unrecognised `Open<ShipmentStatus>` (safe:
  non-terminal), never coerced. The delivery contact (recipient name/phone/address) is VN resident
  personal data transmitted to the courier — a data processor under PDPD, lawful basis contract
  performance — for the sole purpose of delivering, never logged, and never carried back on the tracked
  shipment. The exact Ahamove endpoint strings are pinned in the gated integration lane; `templates/adapter-template`
  is extracted at the third integration adapter (`erp-sap`), not before (the rule of three, roadmap P11).
- `erp-sap` (P11, ADR-0059 — a new ADR): the **`ErpSink`** adapter, the second integration adapter and
  the first that is not a courier — proving the transport-seam pattern generalises past one port shape.
  It maps the port's two operations — post a whole day, read back what posted — onto a REST ERP API
  behind an `ErpTransport` seam (the same shape the couriers use, ADR-0058), with request shaping and
  HTTP→`PortError` mapping pure and the socket behind `TlsErpTransport` (one trusted configured host,
  no SSRF surface). The shared `ErpSink` contract suite runs in the fast gate against a stateful stub
  ERP with no socket, proving the three obligations: idempotency by revision (`store:business_date:revision`,
  a retried nightly job harmless), a higher revision superseding a lower one for the same day (an
  adapter that appended would double-count revenue), and whole-or-nothing validation (an unknown
  account fails the entire batch `invalid_argument` with nothing posted — half a day in a period is
  worse than none). Postings are keyed by the **trading** day (`business_date`), the deliberate
  opposite of `fiscal-vn`'s calendar-date keying. Posting is nightly and off the sales path (ADR-0001),
  and carries no personal data (aggregates by account and day). Two small additive read-only accessors
  land on the port types to let an adapter serialise a line without matching the `#[non_exhaustive]`
  `ErpLine` from outside `pos-ports`: `ErpLine::{kind_wire, amount, quantity}` and `Quantity::as_milli`.
  The exact SAP strings are pinned in the gated integration lane; `erp-sap` is the third integration
  adapter's second divergent prior (with the courier) for the `templates/adapter-template` extraction.
- `shipping-grabexpress` (P11, ADR-0058): the second **`ShippingDispatch`** courier, the other one
  `architecture.md` §6.1 names, built from the new `templates/adapter-template`. Same transport-seam
  shape, same pure status mapping, same stub-driven contract suite as `shipping-ahamove` — differing
  only in Grab Express's own wire (a booking keyed by `merchant_order_id`, a `deliveries` collection,
  its own status vocabulary `ALLOCATING`/`PICKING_UP`/`IN_DELIVERY`/`COMPLETED`/`CANCELED`, mapped onto
  `ShipmentStatus` with unknowns kept unrecognised and non-terminal). It passes the shared
  `ShippingDispatch` suite (all 8 cases) against a stateful stub courier, plus per-status unit tests.
  No new ADR — it is the second instance under ADR-0058.
- `templates/adapter-template/` (P11, roadmap P11 "rule of three"): the extracted scaffold for a new
  network adapter, generalising the pattern now shared by `shipping-ahamove`, `shipping-grabexpress`,
  `erp-sap`, and `cloud-sync-http` — a transport seam (the only thing that touches a socket) plus a
  `Tls…Transport` on the tree's pinned rustls/hyper stack, a pure core (request shaping + status→`PortError`
  mapping), and a stub-driven contract test. It is deliberately **not** a workspace member: the source
  files carry a `.tmpl` suffix and there is no real `Cargo.toml`, so Cargo, `clippy`, `rustfmt`, and
  `cargo test` never see it — a contributor copies it out, drops the suffix, fills the placeholders,
  and adds the crate to `members`. A `README.md` carries the shape, the copy-out checklist, and links
  to the four worked examples.
- `pos-simulator` (P12): the **executable capacity model** (`crates/pos-simulator/src/capacity.rs`), the
  first slice of the virtual-fleet simulator. `docs/capacity-and-reliability.md` §2's three scenarios
  (A/B/C) are encoded as data and its sizing formulas as pure integer functions, so the published
  numbers become a *checked* artifact rather than an estimate on a page: events/day is reproduced
  exactly (recovering the table's own implied 8-events-per-bill constant), PostgreSQL storage within 5%
  (the model gives B 75 GB where the table rounds to 72), daily bandwidth lands inside each published
  range (from QR sessions × ~1 MB of menu imagery — scenario B's ~250 GB/day wall), and the §2
  ÷1260 peak-ingest formula is shown to be a conservative ceiling every scenario sits under. A
  `reconcile()` returns the derived-vs-published divergences instead of hiding them, and pins the one
  place the estimates do not reconcile — scenario A's 9,000 QR sessions/day against the 18,000 the
  bills×QR-share model gives (B and C both agree with the share), filed for the pilot to settle. All
  integer, no floating point on a capacity number; own `[lints]` table like the binaries. The fleet
  behavioral scenarios (OTA rings, offline drain, config fan-out, reconciliation) are the next slice;
  the real sustained soak on the target VPS with live PostgreSQL/NATS is an operations/P13 handoff, not
  faked here.
- `pos-simulator` (P12): the **virtual-fleet behavioral scenarios** (`fleet`, `stress`). `fleet` folds
  the framework's *real* OTA decision — `pos_core::ota::decide_rollout` (ADR-0048) — over a whole fleet
  and returns the aggregate, so the canary ramp, the kill switch reaching every ring, a revoked key
  refused fleet-wide, and a failed self-test rolling one device back are each asserted across the fleet
  rather than one device (the scenarios `crates/pos-core/tests/ota_rollout.rs` seeded now have their
  home). `stress` makes §4's behavioural stress tests executable: the offline drain reproduces the
  "200 stores → 800k events" figure from the fleet model and shows the ~9-minute drain is feasible
  within the ingest ceiling (and conservative); the webhook backpressure model shows a dead endpoint's
  cursor falls behind linearly while its in-memory footprint stays one batch, whatever the outage; and
  the nightly reconciliation is the sorted set-difference (`store − cloud`) of missing ids to re-push.
  17 tests in all; still integer-only and deterministic (rates and counts are inputs, no clock).
- `pos-simulator` (P12): the **runnable entry and reporting** (`report`, `main`, `just simulate`), plus
  the P12 exit recorded honestly. `report` renders the capacity envelope and the reconciliation report
  as text (pure, unit-tested); the `pos-simulator` binary prints them and is the one place `print` is
  allowed. `docs/capacity-and-reliability.md` gains a §8 stating the numbers are now executable and
  self-checked, and pins the one standing discrepancy (scenario A's 9,000 QR sessions/day vs the
  model's 18,000) for the pilot rather than silently changing it. `docs/roadmap.md` records P12's
  status: the deterministic core (capacity model + fleet scenarios) is done, while *"scenario B
  reproduced on target hardware"* and *"the soak runs nightly"* are a pilot/operations handoff (like
  A4/A5), not faked. The same status note records P11's: the unblocked surface is done, and serving the
  intake awaits a cloud→store API-contract decision (the cloud cannot implement `OrderIn`, and the tree
  is store-initiated only).
- `docs/guides/` (P13): four **task-shaped guides**, each finishing in one sitting and each linking the
  worked examples and the CI enforcement it names — [start from zero](docs/guides/start-from-zero.md)
  (run `pos_edge` on a laptop with `just run-edge`, then deploy the cloud tier to one VPS by the
  six-secret sslip.io path), [write an adapter](docs/guides/write-an-adapter.md) (copy
  `templates/adapter-template`, map each provider status to a `PortError`, pass the port's contract
  suite), [add a country module](docs/guides/add-a-country-module.md) (the three ADR-0027 edits that
  `cargo xtask countries` enforces), and [run the simulator](docs/guides/run-the-simulator.md) (`just
  simulate` and the tests behind it). A `docs/guides/README.md` indexes them and states the two-tier
  mental model (store `pos_edge` vs cloud `pos_cloud`). Walking the fork-to-UI checklist against the
  real artifacts is what these guides do, and it surfaced the two `Fixed` items below.
- `docs/roadmap.md` records P13's status: the repository-only half (the four guides, and the fork-to-UI
  walk that produced the fixes below) is done, while *"a pilot store trades a full day"* and the hardware
  matrix exercised for real need physical stores and procured hardware (A5) — a pilot/operations handoff,
  like the WAL-on-Windows soak (A4), that this documentation sets up rather than performs.
- `deploy` workflow: an **optional `VPS_PORT` secret** for a VPS whose SSH is not on 22. It defaults to
  22 when unset or empty (so existing forks are unaffected), and is threaded into every `ssh`/`scp` step
  (`-p`/`-P`). The runbook and Start-from-zero note that `VPS_KNOWN_HOSTS` for a non-default port must be
  generated with `ssh-keyscan -p <port>`, whose entries are keyed `[host]:port` — the form SSH looks up.

### Changed
- **Deploy images are cross-compiled on the runner, no longer built under QEMU emulation.** The
  arm64 support added above built the non-native architecture by emulating the box's CPU
  (`tonistiigi/binfmt` + `docker buildx`), which compiles Rust under emulation — an hour or more,
  and QEMU's occasional miscompiles (illegal-instruction, atomics) made it flaky as well as slow.
  Both Dockerfiles now pin their builder stage to `$BUILDPLATFORM` (the runner's own CPU) and emit
  a `$TARGETPLATFORM` binary: `deploy/Dockerfile` cross-compiles `pos_cloud` for the Rust triple
  chosen from `TARGETARCH`, with the `gcc-aarch64-linux-gnu` toolchain linking `ring`'s C/assembly
  for the arm64 build; `deploy/caddy.Dockerfile` cross-compiles Caddy with `GOARCH`. Both runtime
  stages now run **no** commands (CA roots are `COPY`ed from the builder, the app runs as a numeric
  uid), so no stage of either image is ever emulated — the build is entirely QEMU-free and runs at
  native speed. The workflow drops the `binfmt` install and lowers its timeout from 120 to 60 min.
  The box-architecture detection over SSH is unchanged. No image contents change for an amd64 box.

### Fixed
- **The arm64 cross-compile now has the target libc headers, so `ring` compiles.** The builder
  installed `gcc-aarch64-linux-gnu` with `--no-install-recommends`, which drops the *Recommended*
  `libc6-dev-arm64-cross` — the arm64 target C headers. The cross compiler was present but headerless,
  so an arm64 deploy died in the builder with `ring` failing to compile `curve25519.c` ("fatal
  error: … No such file or directory / compilation terminated"). `deploy/Dockerfile` now installs
  `libc6-dev-arm64-cross` alongside the cross gcc. amd64 builds are unaffected.
- **Deploy now builds images for the VPS's own CPU architecture, not the runner's.** The `deploy`
  workflow always produced amd64 images (`docker build`, `docker pull`), so on an arm64 box — Oracle
  Ampere, AWS Graviton, most free-tier ARM instances — every built/pulled container died with
  `exec /usr/bin/caddy: exec format error` / `Restarting (255)`, and nothing listened on 80/443 (the
  multi-arch official postgres/nats/garage images came up, masking it). The workflow now detects the
  box architecture over SSH (`uname -m` → `linux/amd64` | `linux/arm64`) before building, then builds
  `pos_cloud` and the custom Caddy image with `docker buildx --platform <target> --load` (QEMU via
  `tonistiigi/binfmt` for the non-native arch) and pulls the stock Caddy image with `--platform`. The
  job timeout rises to 120 min because a cross-arch Rust build under emulation is slow. Unsupported
  `uname -m` values fail fast with a clear message.
- `deploy/reset-admin.sh`: the break-glass **no longer errors when the admin tables do not exist
  yet.** It ran `DELETE FROM super_admin; DELETE FROM admin_sessions;` unconditionally, so
  `reset_admin=true` on a first deploy (before the app's first migration created those tables) failed
  the workflow with `relation "super_admin" does not exist` — contradicting the script's own "idempotent
  … still succeeds" promise. Each DELETE is now guarded by `to_regclass`, so a reset before the schema
  exists is a clean no-op. (`reset_admin` is only meant for wiping an *existing* super-admin; a first
  deploy should leave it off and enrol via the setup token.)
- `deploy/bootstrap.sh`: **`cloud.toml` is now readable by the app container on a non-root deploy
  user.** `pos_cloud` runs as uid 10001 and its config is a mode-600 file; bootstrap only `chown`ed it
  when run as root, so a sudo user (the common cloud default — Oracle's `ubuntu`, etc.) left the file
  owned by the deploy user and the container could not read it, so `pos_cloud` would fail to start.
  bootstrap now falls back to `sudo -n chown` when not root (no regression where neither root nor
  passwordless sudo is available — still a warning, not a hard failure). The one-time super-admin
  setup token is captured at cloud.toml creation and printed from that variable, so it still shows
  even after the file is chowned away from the deploy user (previously it was re-read from the file
  afterward, which the chown could make unreadable). Only `pos_cloud` needs this — `postgres` reads
  `pos.env` via the daemon (`env_file`) and the `nats`/`garage` images read their mounts as root.
- **Deploy was broken on the sslip.io path (two ways).** (1) The `deploy` workflow always built a
  custom Caddy image via `xcaddy --with caddy-dns/cloudflare`; against a current plugin that build
  failed on Caddy 2.8.4 with `undefined: zapslog.HandlerOptions` (xcaddy resolved `go.uber.org/zap`
  past the experimental API 2.8.4 references), so *no* deploy could produce images. (2) Even had it
  built, `deploy/Caddyfile` hard-coded a Cloudflare DNS-01 `tls{}` block, so an sslip.io deploy (empty
  `CF_DNS_API_TOKEN`) would fail certificate issuance and never serve `:443` — while the docs promised
  sslip.io "just works". Fixed both: the Caddy image and Caddyfile are now chosen by `DOMAIN` —
  `*.sslip.io` uses the **stock official `caddy:2.8.4`** image and an **HTTP-01/TLS-ALPN** `Caddyfile`
  (no plugin, no Cloudflare, the fragile `xcaddy` build skipped entirely), while a managed domain keeps
  the custom image (now pinned to Caddy `2.10.0`, which builds the plugin against a self-consistent
  module graph) and the DNS-01 `deploy/Caddyfile.cloudflare`. `bootstrap.sh` selects the Caddyfile from
  `DOMAIN` (read back from `caddy.env`, so re-runs are correct); the workflow selects the image and
  carries it as `CADDY_IMAGE`. Docs updated to state both modes. The custom-image (domain) build runs
  only in the manual deploy workflow, so confirm it against a real Cloudflare domain when first used.
- `store-postgres` integration test (`subjects_store::fetch_due_then_mask_is_idempotent`): the seed
  bound a `&str` to a `jsonb` column via `$4::jsonb`, which makes Postgres infer the *parameter* as
  `jsonb` and `tokio-postgres` then rejects the `&str` (`WrongType { Jsonb, "&str" }`) — the seed never
  ran, failing the merge-to-main integration lane against a live PostgreSQL. Changed it to `$4::text::jsonb`
  (pin the parameter to `text`, cast to `jsonb` in the database), the exact idiom every production writer
  already uses — `subjects.rs` (`mask`), `config_trees.rs`, `translations.rs`, `rollups.rs`. Test-only;
  no shipped code changed. The PR-gate CI does not run the Postgres lane, so this surfaced only on merge.
- `justfile` (P13): **`just` was completely broken** — a stale "Development loops" block left duplicate
  `run-edge`/`simulate` recipes and placeholder `run-cloud`/`deploy` bodies, and with no
  `allow-duplicate-recipes` setting `just` refused to parse the file at all, so *every* `just` command
  failed. Removed the duplicates, gave `run-cloud` a real body (it runs `pos-cloud` against a
  `POS_CLOUD_CONFIG` path and names the backends it needs), and replaced the stale `deploy` placeholder
  with a pointer to the deploy workflow and runbook. Found by walking the P13 fork-to-UI checklist.
- `README.md` (P13): the Quickstart listed `VPS_SSH_PORT` (a secret the deploy workflow does not use) and
  omitted `VPS_KNOWN_HOSTS` (one it requires). Rewrote it to run `just run-edge` locally and defer the
  deploy secrets to the single source of truth in [`docs/deploy-runbook.md`](docs/deploy-runbook.md),
  which now leads with the six-secret sslip.io fastest path, so a first-time reader cannot copy a wrong
  secret list. Added `docs/guides/` and `docs/deploy-runbook.md` to the documentation map.
- `pos-core` tests (P9): **OTA rollout scenarios** over a virtual fleet
  (`crates/pos-core/tests/ota_rollout.rs`), the `docs/roadmap.md` P9 exit proof seeded against the pure
  `decide_rollout` ahead of P12's full `pos-simulator`. Five scenarios pin the safety properties with no
  I/O or clock: the fleet canary ramps by bucket while lab and pilot take the update immediately; a
  raised minimum ring holds the lower rings back; a failed self-test rolls back over the kill switch and
  a revoked key both; the kill switch freezes an otherwise-eligible fleet; and a revoked signing key is
  refused fleet-wide even at full ramp.
- `examples/minimal-edge`: the smallest runnable store — `pos_edge` on a fixed dev store id with no
  database, hardware, or config file. `just run-edge` runs it; it grows to compose the edge over
  `pos-fakes` as the P5 domain routes land.
- `cargo-deny` gains two curated, dated `skip`/`skip-tree` entries (`syn@2`, `sha2@0.11`): transient
  duplicates from the ecosystem's mid-migration across major versions, all build-time or
  handshake-only and none changing what a shipped binary links. This is the curation the `deny.toml`
  comment anticipated for when the axum/tokio stack arrived; both entries are reviewed on 2026-11-19.

- `pos-core` permission registry (`pos-spec.md` §9): a **fixed catalogue** of 24 permissions declared
  through one `permissions!` macro, so a new permission is a single entry that cannot omit its group,
  risk, PIN flag, default roles or description — the enum, its `ALL`, and `Permission::meta` all
  derive from that one block and cannot drift. `PermissionSet` is a `u64` bitset (a compile-time
  assertion keeps the catalogue inside 64), roles are data synced from the cloud, and every check
  goes through one `require()` gate that is **deny by default** and returns `DomainError::PermissionDenied`
  naming the id. High-risk money vectors (void, comp, refund, price override, drawer-no-sale) carry a
  mandatory PIN flag a test enforces. `docs/snapshots/permissions.txt` records every id under the same
  removal gate as the event catalogue — the bare id is an immutable contract, the tabbed metadata is
  mutable — and `docs/permissions.md` is the generated role matrix.

- `pos-core` capability context (`pos-spec.md` §10): the ten store-profile flags (`tables_enabled`,
  `kds_enabled`, `pay_first_enabled`, …) as a fixed catalogue declared through one `capabilities!`
  macro, read through a single `CapabilityContext` — `require()` returns `DomainError::CapabilityDisabled`
  naming the key, so the banned "scatter `if flag` through the code" pattern has nowhere to live.
  Full-service, cafe-counter and retail are three presets over the same flags. Inter-flag validity
  (`pay_first` excludes `tables`; `seats` requires `tables`) is `conflicts()`, a pure function over
  enumerable `RULES` the cloud runs before publishing a config version and the edge could run
  identically. `docs/snapshots/capabilities.txt` puts every flag key under the removal gate — a key
  is a config term a synced edge reads — with its `default` as mutable metadata.

- `pos-core` business-date derivation (`pos-spec.md` §14.1, ADR-0014): `derive_business_date` turns
  an instant into the trading day it belongs to, in the **store's** timezone with its cutoff hour
  (default 04:00) — computing rollups in the server's timezone is named in `docs/roadmap.md` P3 as
  *the* classic revenue-skewing bug. It runs the safe direction (instant → civil) and subtracts the
  cutoff as civil arithmetic, so a 25-hour fall-back day needs no special case. `resolve_local_time`
  handles the ambiguous direction (daypart and shift boundaries) with the one policy ADR-0014 fixes —
  a skipped local time resolves forward, a doubled one to the earlier instant. `StoreTimeZone` and
  `CutoffHour` validate at construction so a bad IANA name or hour fails once, not per derivation, and
  `jiff` stays out of the crate's public signatures. Tests cover Ho Chi Minh, Honolulu, and both US
  DST transitions. `pos-core` now enables `jiff`'s `tzdb-bundle-always` feature (ADR-0014), so the
  timezone database is compiled into the binary as pure data — Windows ships none and the edge is a
  static binary on an unadministered machine; this adds to binary size on both tiers, accepted
  because the tablet and the cloud aggregator must apply the *same* rules or one bill lands on two
  different business dates.

- `pos-core` inventory (`pos-spec.md` §8): recipes as a **bill of materials per item and per
  modifier** (a modifier carries its own `MenuItemId` and its own `Recipe`, so "the large size adds
  50 g of dough" is a recipe like any other), a `StockProjection` of on-hand quantities updated by
  the five ledger movements, and `available(item) = floor(min over ingredients(on_hand / per_unit))`.
  Because availability reads the current projection every time, shared ingredients propagate for
  free — the archive's C=10/D=8/E=6 fixture is a test: cooking one A drops B's availability from 8 to
  7 through the shared ingredient D, without B being sold. `Availability::is_sellable` is the auto-86
  decision; `consumption_for_fire` sums base plus modifiers scaled by line quantity; and
  `stocktake_movement` computes the delta against the projection **at count time**, so sales during a
  count are preserved rather than overwritten. All arithmetic is integer `Quantity` in thousandths —
  no float on the availability path.

- `pos-core` campaign engine (`pos-spec.md` §7): one `Campaign` model for happy hours, item and
  category discounts, combos, vouchers and manual reductions, evaluated in §7's **deterministic
  order** (item-level → combo → bill-level → voucher → manual, then by descending priority, then by
  id) with a **split timing** — `evaluate` at `Timing::LineAdd` applies item and combo rules against
  the line, at `Timing::PaymentStart` the bill-level and voucher rules against the bill, so a guest
  who ordered at 16:59 keeps the happy-hour price when they pay at 17:30. The voucher stage is
  skipped entirely when `Connectivity::Offline` — rules run offline, uniqueness runs online.
  Exclusion groups admit only their highest-priority match, quota gates each campaign, schedule
  windows may wrap past midnight, and each applied campaign is its own reduction line computed on the
  running remainder so the total can never exceed the base. Percentage and fixed-amount actions are
  modeled; combo-price and free-item actions wait for the `decide` slice's menu/line model.

- `pos-core` decision spine (`decide(state, command, ctx) -> Decision`, ADR-0013): the sans-I/O
  point where a command meets the domain. `DecisionCtx` is the single place a decision reads ambient
  truth — `now` (read once, a value not a clock), the derived `business_date`, the actor, the granted
  `PermissionSet`, the `CapabilityContext`, connectivity and currency — so "the clock is read once"
  and "flags are read through one surface" are structural, not conventional. `decide_line` wires the
  order-line command family through the state machine (legal transitions), the permission registry (a
  void-after-fire needs `sales.line.void_fired` **and**, because it is PIN-flagged, a verified PIN),
  the capability profile (firing by course needs `courses_enabled`), and inventory (a fire's
  consumption movements). It returns a `#[must_use] LineDecision` carrying the next state, the stock
  ledger writes, and the post-commit `Effect`s (print a void ticket, recheck availability).

- `pos-core` decision spine — the remaining command families. `decide_bill` settles a bill through
  `billing::settle` (the invariant is proven or the command refused) and voids one behind
  `billing.bill.void` + PIN; `decide_shift` runs the **blind** close (§11.1) — the count is recorded
  without the decision revealing the expected total — behind `cash.shift.close`; `decide_table`
  drives the floor cycle (seat → request bill → settle → clean) gated wholesale by the
  `tables_enabled` capability. All four aggregates (line, bill, shift, table) now share one
  `DecisionCtx`/`Effect` spine, so the `decide(state, command, ctx) -> Decision` orchestration is
  complete across the P3 lifecycles.

### Changed
- `README.md`'s repository layout moves country modules out of `crates/adapters/` and up to
  `countries/` at the root. Filing `fiscal-vn` beside `store-sqlite` described a country as one
  implementation of one port when it is five things, and it hid the unit a fork adds or removes.
- `pos-spec.md`: tax is per item class and keyed by sales channel, not a flat store rate;
  a table has exactly one open order; one open shift per cashier device; queue numbers
  reset daily and are not the receipt counter.
- `naming-and-api.md`: the `bills:split` and `webhook_deliveries:redeliver` custom methods.
- Cargo workspace with the three backbone crates, the pinned toolchain, layered
  lints, `deny.toml`, the `justfile`, and the `xtask` crate carrying the repository
  checks: the dependency rule, the per-crate `clippy.toml` baseline guard, action
  pinning, and internal documentation links. Each is proven to fire, not merely
  written.
- CI: a pull-request gate under ten minutes (rules, lints, tests, both build
  targets, licences, secrets, changelog), a merge-to-`main` workflow, a nightly
  advisory scan, and a daily mirror with a deletion-proof bundle.
- `pos-proto` value types, the foundation every later calculation trusts: `Ulid`
  (in-house Crockford base32, injective, time-sortable), `Money` with `Ratio`,
  `Quantity` and a single `div_round` primitive, `Timestamp`, and `BusinessDate` and
  `CalendarDate` as deliberately unconvertible types. Eighteen resource-identifier
  newtypes over `Ulid`, so a `StoreId` cannot be passed where a `TenantId` belongs.
  Fifty-six tests including property tests for the split-rounding law.
- `pos-proto` wire machinery: `Open<E>`, which degrades an unknown enum value to
  `*_UNSPECIFIED` **while retaining the original token**, plus `require()` as the
  domain boundary that refuses it; the `wire_enum!` macro; ten closed vocabularies
  (`OrderState`, `PaymentMethod`, `PaymentOutcome`, `ReductionKind`, …); `NoPii` as a
  sealed marker so text in an event payload is a compile error with an instruction
  attached; and the two determinism traits, `ClockSource` and `IdGenerator`.
- The event envelope, the AIP-193 error envelope with its nine canonical statuses, the
  `PROTOCOL_VERSION` handshake, and the full event catalogue: **49 types**, being the 38
  the specification declares plus 11 that stated rules needed and nothing carried.
- `docs/snapshots/events.txt`, generated from the catalogue, with a CI gate that refuses
  any removal — a published event type or payload field is a contract.
- Four narrow text types (`DisplayName`, `TranslationKey`, `PermissionKey`,
  `ReleaseTag`), each admissible in an event payload for a stated reason, and
  `GuestNote`, which deliberately is not.

### Changed
- **`pos-spec.md` §18 now lists 49 event types.** The eleven additions are tabulated with
  the rule each one serves; the sharpest is `security.permission.overridden`, since a
  manager-PIN override above a discount ceiling is a named fraud control that had no
  auditable record at all.
- **`pos-spec.md` §3: a line note's text never enters the event log.** It is where "for
  Mr Nguyễn, severe peanut allergy" gets typed — a name and a health condition — and the
  log is immutable, so nothing personal in it could ever be erased. Events carry only
  whether a note exists; the kitchen reads it from the local order record.
- Every document now carries the mandatory `Status` / `Owner` / `Last reviewed` header that
  `engineering-guide.md` §12b requires.
- `architecture.md` §5 is now the authoritative port table and lists **sixteen** ports.
- `engineering-guide.md` §8's ADR index reached only 0009; it now covers every record.

### Fixed
- **The dependency rule reported crates that are never linked.** Reading
  `cargo metadata`'s resolve graph reported `log`, `defmt` and `bitflags` behind
  `jiff`, none of which this workspace activates, and reading `cargo tree` instead
  reported `syn` and `quote`, which run inside the compiler. The check now uses the
  metadata graph for structure, follows an edge only when `cargo tree` says it is
  activated, and stops at procedural macros — so the allow-list stays a statement
  about runtime dependencies rather than accumulating build-time noise. Two tests
  pin both halves.
- **`OrderIn` was missing from the port list.** ADR-0006 and `architecture.md` §5 both named
  fifteen ports and omitted it, although ADR-0012 and `pos-spec.md` §13 depend on it — it is
  the reason QR ordering reuses the marketplace intake path instead of adding a pipeline.
  ADR-0021 supersedes ADR-0006 with the corrected list.

### Upgrade notes
- Documentation and decisions only; no code, no protocol, no migrations, no permission
  changes. ADR-0006 is marked superseded rather than edited — its decision stands, only its
  port list was incomplete.
- The permission catalogue introduces 24 permission identifiers (`docs/snapshots/permissions.txt`).
  This is the initial catalogue, not a change to an existing one, so nothing needs migrating; but
  from here on adding, retiring or re-defaulting a permission is an `Upgrade note` under rule 4, and a
  role synced from an older cloud that still names a retired id must keep resolving — ids are
  deprecated, never removed.
- `CODEOWNERS` routes review to four `@maintainers-*` teams. GitHub **silently ignores** an
  entry naming a team that does not exist, so the required-review protection on the backbone
  crates does nothing until those teams are created. See `MAINTAINERS.md`.

---

## Template for a released version

```markdown
## [1.4.0] — 2026-09-01

**Product version** 1.4.0 · **Protocol version** 3 · **MSRV** 1.83
**For restaurant staff:** split bills now always add up to the original total; nothing else changes on screen.

### Added
- Seat-level ordering behind the `seats_enabled` capability flag. (#204)

### Fixed
- Rounding remainder on uneven bill splits is assigned to the final split. (#231)

### Upgrade notes
- Migration `0042_add_seat_to_order_lines` is additive; rollback to 1.3.x is safe.
- New permission `sales.order_line.assign_seat` is granted to the Server template by default.
- No protocol change; cloud 1.4.0 serves edge 1.2.x and 1.3.x.
```
