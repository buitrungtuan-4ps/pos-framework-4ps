# Cloud admin console — complete overhaul plan (v2)

**Status** Proposed · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-26

Version 2 of the console plan. Version 1 (the A0–A8 roadmap) framed the problem — one context
contract, everything as master data, one screen pattern — and it survives here intact. What changed:
v2 is grounded in an **exhaustive code audit** (8 parallel review passes, 154 findings, every one
cited to file and line) that measured the console against a framework-standard multi-tenant admin
(Shopify admin / Stripe dashboard / Toast back office class). The audit found that A0–A8 under-scoped
three whole pillars — **fleet observability, audit trail & multi-admin identity, and the media/file
rail** — and surfaced **seven real functional bugs** to fix immediately. This document is the complete
plan: the verdict, the bugs, the gap map, the target standard, and a re-cut roadmap in four tracks.

Companions: `docs/roadmap.md` (runtime to pilot-ready — done), `docs/ui-ux.md` (design principles).
Each phase lands as reviewed PRs; contract changes get ADRs.

---

## 1. Verdict — the direct answer to "is it complete?"

| Question | Verdict | Evidence (from the audit) |
|---|---|---|
| **Dễ sử dụng (usable)?** | **No — internal-ops-tool maturity.** | No screen loads data on mount (every screen sits blank behind a manual "Load" button); zero confirm dialogs anywhere (revoke key / delete webhook / reject device fire on first click); one shared `busy` flag disables every button on a screen during any single call; editing panels sit at the bottom of a card with no scroll-into-view; no toasts — outcomes vanish on navigation. |
| **Master data đầy đủ (complete)?** | **No — 2 of 12 domains.** | Present: Organization, Catalog & pricing. Missing entirely despite the runtime already supporting them: employees + PIN, roles/permissions, campaigns/vouchers, inventory/BOM/86, floor plan & tables, kitchen stations & printer routing, tax-rate tables, receipt templates/branding, payment methods, channel policies, suppliers (light), multilingual names. |
| **Tính năng CRUD đầy đủ (dropdown, drag-drop, edit/add/delete…)?** | **No.** | Dropdowns: yes, pervasive (ADR-0065 killed ULID entry). Drag-and-drop: **zero in the whole app** — every ordering is a hand-typed integer, and the Layout editor designs a POS touch screen with **no visual grid at all**. Search/filter/sort/pagination: **zero on any list** (client and server). Bulk actions: none. Delete: no confirmation, no undo. Read-one endpoints: none. Update: full-replace PATCH, last-write-wins, no concurrency token. |
| **Admin nắm được toàn hệ thống, xử lý kịp thời?** | **No — effectively blind.** | The cloud never observes store liveness (no heartbeat, no last-seen column anywhere); no fleet-health screen; **0 of 6** of the archive's mandatory minimum alert set implemented; no alerting channel of any kind; relay queue depth invisible; OTA has no report-back (the cloud never learns what version a device runs); reconciliation has **no edge caller** — it never actually runs. |
| **Audit trail đầy đủ?** | **No — none.** | No audit table in any of 17 migrations; ~60 admin write routes record nothing; **one anonymous super-admin enforced at schema level** (`boolean PRIMARY KEY CHECK (id)`), so attribution is impossible by construction; catalog UPDATEs destroy prior values (a price change is unrecoverable); config versions keep history but expose no author, no browse, no diff, no rollback endpoint. |
| **Performance?** | **Built to a "tens of rows" assumption.** | ~20 unbounded list endpoints (no LIMIT anywhere); full-tenant refetch of six collections after every single mutation; 10k items would ship the whole master per request and render 100k+ DOM nodes; projector does a serial O(stores) sweep plus a `SELECT DISTINCT` full scan every 30 s; no compression/timeout/body-limit layers in the binary; no cache headers on hashed assets. Good bones: materialized rollups (<10 ms reads), parallel in-screen fetches, tenant indexes, 216 KB bundle. |
| **International adaptive?** | **Foundation strong, multi-country thin.** | Genuinely bilingual (en+vi at 100% key parity, ICU runtime, CI-enforced string extraction). But: locale not persisted (resets to en every load); money formatting breaks for fractional currencies; currency is a free-text field; zero date/timezone handling; translation-grid columns hardcoded to en/vi (cannot add ja/ko); the whole `pos-country` locale-pack framework is invisible to the cloud; no per-locale item names; no RTL. |

**Overall:** the delivery rails (config tree, publish, compile, event log, rollups) are
production-shaped; the console on top of them is roughly 30% of a complete admin dashboard.

---

## 2. Fix now — real bugs the audit found (F0)

Not UX opinions; functional defects, each small enough to fix immediately:

1. **A tenant cannot be created from the UI at all.** `api.createTenant` exists in the client and is
   called by zero screens; the ContextPicker's empty state offers no create. First-run is a dead end
   without curl. *(ContextPicker.tsx, client.ts)*
2. **The context-gate ULID error** (v1's A0): scoped screens fire requests with an empty tenant/store
   id and surface `… is not a ULID`. One shared guard + "choose a store to continue". *(session.ts)*
3. **New-store wizard duplicates stores**: Back from step 2 then Next calls `createStore` again with
   no idempotency guard. *(NewStore.tsx:64-81)*
4. **Session death mid-edit is unhandled**: `ApiError.isUnauthorized` is defined and consumed
   nowhere; when the absolute 8 h TTL lapses, every action fails with a raw "unauthorized" banner
   instead of a re-login redirect preserving work. *(client.ts)*
5. **Webhook re-enable is impossible**: the dispatcher auto-disables a failing endpoint (correct) but
   no admin route or button can re-enable it — delete-and-recreate rotates the secret. *(http.rs)*
6. **No screen auto-loads** — add fetch-on-mount everywhere (with loading skeletons), retire the
   "Load" buttons.
7. **Static assets ship with no cache headers** despite content-hashed filenames — one line
   (`cache-control: immutable`) per hashed asset. *(assets.rs)*

Two larger correctness gaps discovered by the audit are scheduled into their tracks below, flagged
here because they are silent no-ops today: **published capability flags never reach the running
store** (`session_from_config` rebuilds only the `menu` node, so flag publishes do nothing → M8), and
**QR table tokens cannot be minted outside tests** (no admin route/UI calls `mint_table_token`, so QR
ordering is unusable in production → M2).

---

## 3. Gap map — what the audit measured, per pillar

154 findings across 8 dimensions. Summary per pillar (details in the audit digest; every finding has
file:line evidence):

### 3.1 Screen interactions (22 findings)
Present: dropdown pickers everywhere, inline rename, archive/restore, per-channel price sheet, empty
states, a genuinely good 3-step New-store wizard, WCAG-audited tokens.
Missing, cross-cutting: auto-load on mount · confirm dialogs (zero) · search/filter/sort/pagination
(zero) · bulk actions · optimistic updates (full refetch per mutation; one `busy` flag freezes the
whole screen) · field-level validation (all errors are one shared banner showing raw backend strings)
· drag-and-drop (zero; all ordering is typed integers) · modals (zero; bottom-of-card editor panels
with no focus move) · Enter-to-save/Escape-to-cancel on inline edits · ULIDs still *displayed*
pervasively (and are the *only* identity for API keys and device proposals). The Catalog screen is an
1801-line single page stacking nine editors; the Layout editor has **no visual preview** of the grid
it edits.

### 3.2 Admin API (27 findings)
Dominant pattern: create + unpaginated tenant-scoped list + full-record PATCH, archive via status.
Missing: read-one on **every** entity · pagination/limit/cursor on **every** list · server-side
search/filter/sort · batch/bulk (incl. bulk price update; publish pays an N+1 per-menu placement
loop) · structured error envelope (admin errors are plain-text sentences; the AIP-193 envelope exists
for /v1 but not /admin) · partial PATCH + ETag/If-Match (all writes are last-write-wins) · API-key
names/labels · webhook update/rotate/ping/enable · activation-code listing · config version
list/diff/rollback routes · the /admin surface in OpenAPI (no machine contract, no drift gate).

### 3.3 Master data (17 findings) — the runtime is dramatically ahead of the console
pos-core ships a **complete permission catalogue (23 permissions, role matrix)**, a **finished
campaign engine (5 kinds, schedules, quotas, vouchers)**, **full inventory (per-item and per-modifier
BOM, five ledger kinds, auto-86)**, and table/station flows — none of it authorable. The edge
hardcodes an 8-table floor and station "S01" *"until the store's real layout syncs from config"*.
TaxClass is a name-only label; the per-(class × channel) rate table has no editor and is never read
by the edge session. BrandRecord is name+status — no logo, no receipt template. Payment methods,
channel enablement, vendor policies: config-JSON-only theoretical paths, nothing validated, nothing
read. Suppliers: unmodeled (lightweight reference only — full purchasing stays ERP territory, spec
§19). Customers/loyalty: correctly out of scope.

### 3.4 Observability & alerting (24 findings)
The only working view is per-store daily event counts behind a manual Load. Missing: fleet-health
overview · store online/offline & last-seen (never observed) · the six-item minimum alert set (0/6:
store offline, e-invoice backlog, invoice range nearly exhausted, disk, clock drift, print-error
spike) · any notification channel (email/webhook/Zalo/Telegram/in-console) · config-version-held per
store (the pull protocol carries it; the handler discards it) · relay queue depth/age per store · OTA
report-back (CloudSync exposes only activate/fetch_update — the cloud never learns installed
versions or self-test failures) · reconciliation scheduler and **any edge caller** · webhook delivery
lag (cursor is in the wire type, never rendered) · JetStream 80% capacity check (primitive exists,
never called) · clock-drift alarm (computed, delivered to no one) · remote log tail · real-time
updates in the console (no WS/SSE/polling) · background-task health surfacing · admin-action audit.

### 3.5 Audit trail & identity (12 findings)
Single anonymous super-admin **enforced by schema** · no audit_log table · no actor parameter on any
store seam (`authenticate_session` returns `()`) · catalog UPDATEs overwrite in place, placements
hard-DELETE · timestamps exist in every table but no Rust record surfaces them · config history is
append-only (good foundation) but exposes no author/browse/diff/rollback · sessions have no listing
or revocation, no IP/UA, absolute TTL only · device approve/reject records when, never who · the
break-glass reset leaves no in-database trace. Contrast: the *domain* event log is audit-grade — the
pattern exists in this codebase; it was never applied to the control plane.

### 3.6 Performance (22 findings)
Good: rollup reads, incremental projector cursor, tenant-scoped + partial indexes, cheap session
check, small no-VDOM bundle, compile-at-publish. Scale blockers: unbounded lists end-to-end ·
full-tenant refetch per mutation · items table renders every row (~10 interactive elements each) ·
O(n) `find()` name resolution per cell (O(items×placements) renders) · projector serial sweep +
`SELECT DISTINCT` over the events table every 30 s (should read the registry) · rollup blob keeps
every trading day ever, rewritten whole every pass, shipped whole to Reports (no date range) ·
compiled MenuBook duplicated into every store's config blob (multi-MB at 10k items; no delta) · no
compression/timeout/body-limit tower layers · no asset cache headers · no code splitting · `ORDER BY
created_at` uncovered by indexes · context picker refetches the world on every open · zero request
metrics.

### 3.7 i18n & multi-country (20 findings)
Present: ICU runtime with typed keys, en+vi at 100% parity with idiomatic Vietnamese, language
switcher, en-fallback floor, CI string-extraction lint, Vietnamese glyph coverage. Missing: locale
persistence/detection (resets to en) · endonym labels ("Tiếng Việt") and localized `<title>` ·
locale-pack-aware money (fractional currencies render in minor units; currency is free text; prices
typed as raw minor integers) · any date/timezone handling · dynamic translation-grid locales (en/vi
hardcoded — ja/ko invisible) · country modules/locale packs surfaced in the cloud (the Rust framework
exists; pos_cloud builds no CountryRegistry) · tax-rate table editor · per-locale item/menu names ·
translated names flowing into the compiled MenuBook · CSV import/export + completion % for the grid ·
RTL (logical properties) · Intl.Collator sorting.

### 3.8 Completeness critic (10 findings the first seven passes missed)
Tenant-creation dead end (above) · PDPD/GDPR **data-subject request tooling** (the masking machinery
exists; there is no lookup/export/erase-by-subject surface — rights PDPD Decree 13/2023 grants) ·
**zero file I/O platform-wide** (no upload or CSV/XLSX export anywhere; spec §16's menu import
unimplementable) · **the ADR-0042 image pipeline is dead code** (no upload route, no storage wiring,
no image field on CatalogItem, no media UI) · session-security residuals (absolute TTL, no sliding
renewal, no security headers, no TOTP recovery, unauth handling unused) · no global search / command
palette / keyboard-shortcut layer (mouse-only console) · no scheduled/effective-dated publishes (a
Tet menu needs a human awake at midnight) · no toast/notification-center primitive · no in-app help
or version visibility (five operator guides ship in-repo, linked from nowhere) · responsive
foundation present but thinning (context picker unusable at 360 px).

---

## 4. The target standard — the console contract

One sentence per rule; every phase below builds toward all of them.

**Interaction contract (every screen):** loads on navigation (skeleton, no Load button) · every list
is searchable, sortable, filterable, paginated (server-side), virtualized past ~200 rows · every
entity follows List → Detail (tabs: related entities + audit trail) → Edit (field-level validation,
partial save, optimistic concurrency) · every destructive action confirms (typed-name confirm for
high-risk) and archives rather than deletes where the domain allows · everything orderable is
drag-and-drop with live preview (layout grid, menu sections, taxonomy) · ids render as names that
link; ULIDs live in a copyable "technical details" disclosure · outcomes are toasts + a notification
center, not vanishing banners · Ctrl/Cmd-K command palette (jump to entity/screen) · keyboard
complete (Enter saves, Escape cancels, dialogs trap focus) · responsive to 360 px.

**Data contract (every entity):** master data, versioned, validated, delivered over the config
tree/publish rails · read-one + paginated-list + partial-PATCH + archive endpoints · created/updated
at/by surfaced · every write audited (actor, action, old → new) · importable/exportable (CSV/XLSX)
where tabular · effective-dating for anything price- or menu-shaped.

**Operations contract:** the fleet's live state is one screen away (online/offline, last sync,
config version held, queue depth, device versions) · every documented alert exists, is stored, and
reaches an admin through at least one channel · every remediation lever in the server has a button
(rollup reset, webhook re-enable, OTA kill switch, config rollback) · the console itself is observed
(request latencies, task health).

**International contract:** locale persists per admin and is detected on first visit · a new locale
is a data drop, never a deploy (dynamic grid columns, per-locale entity names) · money always renders
via the store's locale pack (currency, exponent, separators) and is entered through a currency-aware
field · times render in the store's timezone with the business date · countries/locale packs/tax
tables are managed master data · layouts tolerate +30% text and are logical-property-ready for RTL.

**Performance targets (NFRs):** any list P95 < 500 ms server-side at 10k items / 1000 stores (≤100
rows/page) · screen interactive < 1 s on the pilot VPS · one mutation refetches only what changed ·
projector tick O(changed stores), not O(events) · publish does not materialize a full tenant ·
per-route latency histograms exist before the fleet does.

---

## 5. The roadmap, re-cut — four tracks

v1's A0–A8 maps into this; nothing is dropped. Sizes: S ≈ one PR · M ≈ a few · L ≈ many.
**Recommended order: F0 → F1 → F2 → G1 → O1 → G2 → M1 → M8 → M2 → O2 → M4 → M5 → M3 → O3 → M6 → M7 → O4 → P2 → F3.**
(F-track first because every later screen is built from its parts; G1 before G2 because audit needs
actors; O1 early because fleet blindness is the operational risk.)

### Track F — Foundations (was A0/A1/A2)

| Phase | Scope | Size |
|---|---|---|
| **F0 · Fix now** | The seven bugs of §2: tenant-create UI, context gate, wizard idempotency, session-expiry redirect, webhook re-enable (route + button), auto-load everywhere, asset cache headers. | **S–M** |
| **F1 · Console shell** | Grouped scope-aware nav (§4 of v1), URL-encoded context (`/t/:tenant/…` + `?store=` — see the note below), breadcrumbs, org switcher with search + caching, toast + notification-center primitives, command palette (screens + entities), locale persistence + endonyms + localized title, in-app help links to the shipped guides, version footer. | **M** |
| **F2 · CRUD kit + API foundation** | Components: DataTable (server search/sort/pagination, virtualization, bulk-select), FormField (label+control+field error, aria-invalid), ConfirmDialog (typed-name for high-risk), Modal/Drawer, StatusBadge, EmptyState, dnd-list primitive, "technical details" ULID disclosure. API: pagination/filter/sort/q params + read-one on every entity; true partial PATCH + ETag/If-Match; AIP-193 structured errors on /admin; API-key labels; `(tenant_id, created_at)` indexes; compression/timeout/body-limit layers; /admin in OpenAPI with drift gate. Migrate Stores, Devices (merge proposals + registry into one Devices area), ApiKeys, Webhooks, Translations onto the kit. Perf wave 1 lands here. | **L** |
| **F3 · Catalog & Layout rebuild** | Split the 1801-line Catalog into kit-based sub-screens (Items / Menus / Modifiers / Taxonomy / Tax classes) with search + bulk price editing; rebuild Layout as a **visual drag-and-drop grid** with device-shaped preview, collision detection, copy-between-channels; drag-to-reorder sections and taxonomy; currency-aware price fields. (Sequenced last in F because it consumes everything F2 builds.) | **L** |

### Track G — Identity & audit (new — was missing from v1)

| Phase | Scope | Size |
|---|---|---|
| **G1 · Multi-admin + console RBAC** | `admin_users` (id, email, name, role, status, per-user password/TOTP) replacing the single-row `super_admin`; invitation flow; console roles (owner/admin/ops/viewer, per-tenant scoping — reuse the §9 registry pattern); sessions gain admin_id/IP/UA, listing + revocation, sliding TTL + idle timeout; login rate-limit; security headers; TOTP re-enrolment + recovery codes. ADR supersedes ADR-0034. | **L** |
| **G2 · Audit trail** | Append-only `audit_log` (actor, action, entity type+id, old→new JSON, at, request id); actor threaded through every store seam; audit tab on every Detail view + global filterable Audit screen; config version history **list/diff/rollback** endpoints + UI (the `effective_at` domain method finally exposed); catalog price-change journal; `created/updated at/by` surfaced on all records; resolved_by on device proposals; break-glass reset writes a tombstone record. | **L** |

### Track M — Master data completion (was A3–A6, expanded)

| Phase | Scope | Size |
|---|---|---|
| **M1 · People & access** (A3) | Employees (name, code, per-store assignment, Argon2id PIN set/reset — T1 PII, PDPD-scoped), role templates over the pos-core catalogue, per-store grants; publish to a `permissions` config node; **edge applies it** (EdgeSession gains the permission set). | **L** |
| **M2 · Floor & kitchen** (new) | Areas/tables master data + **visual floor editor (drag-drop)**; table QR token minting + printable QR sheets (wires the orphaned `mint_table_token`); kitchen stations + item→station routing rules + backup-printer fallback; publish to `floor`/`stations` nodes; edge reads them (kills hardcoded FLOOR/S01). | **L** |
| **M3 · Campaigns & scheduling** (new) | Campaign/promotion/voucher authoring over the finished pos-core engine (5 kinds, windows, quotas, exclusions); voucher batch generation; **effective-dated & scheduled publishes** (menu/config/campaign — the Tet-menu case); publish preview/diff. | **L** |
| **M4 · Localization & tax** (A4) | Countries/locale packs surfaced as master data (pos_cloud builds the CountryRegistry); per-(tax class × channel) rate-table editor, validated and **read by the edge**; currency picker + locale-pack money entry/formatting; store timezone + business-date display; dynamic translation-grid locales + completion % + missing-only filter; per-locale item/menu names flowing into the compiled MenuBook; receipt templates + brand logo/footer (consumes M5). | **L** |
| **M5 · Media & file rail** (new) | Upload route + object-storage wiring for the existing ADR-0042 image pipeline; image fields on items/brands; media library UI; **CSV/XLSX import/export rail** (items, placements/prices, translations, employees, reports) with dry-run validation report; PDPD subject-request tooling (lookup/export/erase by subject id, itself audited). | **M–L** |
| **M6 · Inventory & suppliers** | Ingredients + units, recipe/BOM editor per item and modifier, auto-86 thresholds, lightweight supplier reference on receipts (full purchasing stays ERP, spec §19). | **L** |
| **M7 · Channels & payments** (A5) | Per-store channel enablement, payment-method/tender configuration, QR guardrail form editor (business hours, rate limits, staff-confirm), marketplace vendor policies (86-handling, throttling); terminal config gated on Track A A1. | **M** |
| **M8 · Config without JSON** (A6) | Form-driven capability editor (toggles + presets + inter-flag conflict preview inline), per-level authored-document read-back, diff-before-publish; **edge applies every structured node** (capabilities today are a silent no-op — session_from_config must rebuild CapabilityContext, rates, and future nodes, not only `menu`). | **M** |

### Track O — Observability & operations (new — was one line in v1; plus A7)

| Phase | Scope | Size |
|---|---|---|
| **O1 · Fleet liveness + overview** | Record last-seen + config-version-held on every store pull (the handler currently discards it); lightweight edge heartbeat; **Fleet home screen**: stores online/offline, last sync, version held vs published, relay queue depth + oldest-pending age, drill-down Store detail page (devices, health, config, recent activity); background-task health endpoint (cursor lag, last tick); console polling/SSE for live refresh. | **L** |
| **O2 · Alerting** | Alert engine + storage + delivery (in-console notification center + email/webhook channel; Zalo/Telegram adapter seam). Implements the six-item minimum set: store offline > 5 min; sync/e-invoice backlog; invoice-range nearly exhausted (when fiscal lands); disk low; clock drift (wire the computed-but-unread `Drift::Alarm`); print-error spike (mined from the event stream). Plus JetStream 80% (call the existing `capacity()`), webhook-endpoint auto-disable notices, projector failure streaks. | **L** |
| **O3 · Sync & OTA closure** | ADR: extend CloudSync with `report()` (installed version, self-test outcome) — the cloud finally learns ring progress; OTA progress UI + kill-switch button (no more hand-editing JSON); reconciliation scheduler + **edge caller** (it has never run end-to-end) + results history UI; remote last-30-minutes log tail over NATS; rollup-reset and other levers get buttons. | **L** |
| **O4 · Reports & analytics** (A7) | Date-range + windowed rollups API (stop shipping all history); revenue/product-mix rollups (extend the projector; prices are T2 — role-gated); charts + CSV export; cross-store comparison; X/Z-report semantics per the spec-gap issue. Perf wave 2 lands here: projector reads the registry (not `SELECT DISTINCT` events), dirty-marking, windowed blobs, config-blob delta, request-latency histograms. | **L** |

### Cross-cutting (holds for every phase)
i18n for every new string (en+vi minimum) · WCAG-AA + contrast gate · audit events from G2 onward ·
OpenAPI + drift gate for every new /admin route · pagination on every new list · docs + CHANGELOG in
the same PR · PDPD posture for T1 (employees, subjects) and T2 (prices, vendor terms).

### v1 → v2 mapping
A0→F0 · A1→F1 · A2→F2 · A3→M1 · A4→M4 · A5→M7 · A6→M8 · A7→O4 · A8 (shared web-kit + branding) →
folded into F2/F3 (kit) and M4 (branding); the shared-package extraction decision (Fork A) is
unchanged and lands when the kit stabilizes.

---

## 6. Decisions to confirm

- **Fork A — shared web-kit** for `ui/` + `dashboard/`: extract after F3 stabilizes (unchanged
  recommendation).
- **Fork B — master-data order**: recommended M1 → M8 → M2 → M4 → M5 → M3 → M6 → M7 (people and
  config-that-actually-applies first; media before localization's logo/receipt needs; inventory and
  payments last). Confirm or reorder.
- **Fork C — incremental refactor** (screen-by-screen onto the kit) over parallel rebuild:
  unchanged; F-track is designed for it.
- **Fork D (new) — alert delivery channel**: in-console + email first, or in-console + Zalo/Telegram
  first (the archive names Zalo/Telegram for VN ops)? Recommend in-console + webhook (generic) first,
  channel adapters after.
- **Fork E (new) — G1 identity scope**: console accounts only (recommended — store staff already have
  the edge PIN system), or one unified identity for console + store staff?

## 7. Immediate next step

**F0** — all seven fixes are small, independent, and testable; it removes the reported ULID error,
the tenant-creation dead end, and the duplicate-store trap in one PR. F1 and F2 follow. The full
sequence is §5's recommended order, re-confirmable at each track boundary.


## Correction — the URL shape, on building it (2026-09-03)

This plan specified `/t/:tenant/s/:store/…`. Built as `/t/:tenant/…` with the store as an optional
`?store=` instead, on the owner's call after the screens were measured.

The store is not a property of a screen the way the tenant is. Fifteen screens read a store and
thirteen never do — but the split that matters is a different one: several of the fifteen work
*with or without* one. People renders its employee table before a store is chosen and uses the store
only to scope the assignments section; Reports, Campaigns, Channels, TaxRates and Config are the
same shape. A required `/s/:store` segment would have forced those to either demand a store they do
not need — a functional regression on screens already reviewed — or carry a sentinel like `/s/-/`,
putting a placeholder where a real id goes. That is the ULID-in-the-UI problem slice 3c existed to
remove, reintroduced in the address bar.

The tenant stays a path segment because it genuinely is required: every tenant-scoped screen needs
one, which is what `RequireContext` gates on. An optional thing belongs in the query, where absence
is the natural state and no placeholder is needed.

What this delivers is what the plan wanted: a link that opens on the tenant it was read under, and
two tabs on two tenants — which `localStorage` context could never do, being per-origin.
