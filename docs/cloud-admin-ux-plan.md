# Cloud admin console — complete overhaul plan (v2)

**Status** Proposed · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-09

Tracks F, G, M and O have since been delivered; §1's verdict table is the state of the console
as v2 found it, not as it stands. **[Wave 3](#wave-3--measured-on-a-running-console-2026-09-09)**
at the foot of this document is the current assessment — written from the running product
rather than from the code — and carries the plan that is live.

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

> **These `O` ids are this plan's own.** [`roadmap-v3.md`](roadmap-v3.md) §A·P4 uses `O1`–`O5` for a different
> set — alert-delivery webhook, printer transport, WAL shipping, JetStream probe, `/internal` auth — and always
> writes them with the `A·P4` prefix; [`production-readiness.md`](production-readiness.md) uses `O1`–`O6` for a
> third. A bare `O2` is ambiguous across the three; say which document you mean.
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

---

# Wave 3 — measured on a running console (2026-09-09)

Everything above was written from the code. This section was written from the **running product**:
`pos_cloud` on a real PostgreSQL 16, the console reached in Chromium, and a first-run operator's
journey walked from an empty database to a store holding an installer file. Twenty-two screenshots
and a request-by-request log per screen are the evidence; what follows is what they showed and what
to do about it, in the order the owner asked for — **mandatory before optional, along the path a
store actually takes to go live**.

## 3w.0 How it was measured, and what the measurement cannot see

One `pos_cloud` process (`bind = 127.0.0.1:8080`) against a local PostgreSQL cluster, all 40-odd
migrations applied clean. The console was driven two ways: through the `pnpm dev` proxy, which is
how a developer sees it, and against `pos_cloud`'s own embedded build, which is how an operator sees
it. Every step recorded a full-page screenshot, the browser's console errors, and every HTTP
response ≥ 400.

Three honest limits on the evidence:

- **No NATS and no Garage.** Docker Hub is unreachable from the environment this ran in, and both
  are optional at boot (`if let Some(nats) = config.nats`, `Some(artifacts) =>`). So the ingest
  cursor, the artifact route and the OTA download path were off. Nothing that depends on them was
  exercised.
- **No store box.** No `pos_edge` was installed against the provisioned store, so Fleet, OTA,
  Reconciliation, Devices and Activation were seen only in their never-reported state.
- **No volume.** One tenant, two stores, an empty catalogue. Every table was seen empty. Nothing
  here measures a `DataTable` at ten thousand rows; that needs seeded data and is listed as work,
  not as a finding.

What the walk *does* cover completely is the mandatory activation path — sign-in, first-run
enrolment, tenant, brand, store, API key, installer handoff — and the empty/first-run state of all
thirty screens, which is precisely the state a new country cell or a new franchisee starts in.

## 3w.1 Five defects, each reproduced

These are not design opinions. Each was reproduced against the running system, and each sits on or
beside the mandatory path.

### D1 · An invited admin can never sign in (blocks the whole multi-admin console)

Reproduced end to end: `POST /admin/invites` returns a single-use token; the invitee opens
`/invite?token=…`, chooses their own password, and the console answers **"Account created"** and
issues a TOTP secret. They then go to sign in and get **"The password or code was not accepted."**
Every time.

The cause is in `crates/pos-cloud/src/auth/admin.rs`. `login` reads `store.load_credential()` — the
*single* super-admin credential — and checks `admin.credential.password_matches(...)`. It never
reads `admin_users.password_phc`, which is exactly where `admin_accept_invite` wrote the invitee's
password (`crates/pos-cloud/src/http.rs`, `create_admin_user`). `LoginRequest` has no email or
identity field at all, so the server could not tell one admin from another even if it wanted to. The
code says so itself, at `admin.rs:773-774`: *"Email-based per-admin login replaces this owner lookup
in a later slice."* That slice was never built.

The consequence is larger than one screen. The Admins roster, the invitation flow, the four console
roles, the per-permission guards, the session list, the audit trail's actor column — Track G in
full — all rest on there being more than one admin, and there cannot be. The console has exactly
one usable account, and the invite flow reports success while leading to a wall.

Two smaller faults ride along on the same path. The enrolment URI handed to the invitee reads
`otpauth://totp/Pizza4Ps:super-admin?...` — every invitee's authenticator app labels the entry
`super-admin`, so a person who administers two cells gets two identically-named entries. And the
sign-in screen has no identity field to fill in, which is the visible symptom of the same gap.

### D2 · The top bar can name the wrong store

Reproduced with two stores in one tenant. Pick *4P's Le Thanh Ton* in the context picker; then open
a link naming *4P's Ben Thanh* — a shared console URL, a bookmark, a colleague's paste. The screens
read Ben Thanh (`localStorage pos.dashboard.store` = Ben Thanh's id, and every request carries it),
and the top bar still reads **"4P's Le Thanh Ton"**.

`dashboard/src/state/session.ts` has two ways in. `selectStore(id, name)` — the picker's path —
writes both the id and the name. `setStoreId(next)` — the *URL's* path, called from `TenantContext`
in `App.tsx` — writes the id and neither writes nor clears the name. Nothing else resolves a name
from an id. So a store arriving by link inherits whatever name the last picker click left behind.
`setTenantId` has the identical shape, so the same applies one level up.

This is the most consequential defect of the five. Configuration publishes, till retirement, tax
authoring and revenue reads are all store-scoped, and the label above them can belong to a different
shop. The feature that makes it reachable — shareable context links — is the one F1 was built for.

### D3 · Creating the first tenant leaves the address bar behind

The picker's `chooseTenant` calls `goToContext(...)`, which is the function whose own comment reads
*"Moving the context moves the URL: the address bar is what a link is copied from."* The picker's
`createTenant` does not call it. Reproduced: create the first tenant and the URL stays at `/` while
the breadcrumb, the nav dots and every screen behave as though a tenant is selected. The state is
right; the address bar is a lie, and a fresh install's very first action produces it.

### D4 · The Admins screen is a blank page under `pnpm dev`

`vite.config.ts` proxies `"/admin": "http://127.0.0.1:8080"`. Vite matches that as a **prefix**, and
the console's admin-roster route is `/admins`. So `GET /admins` is forwarded to `pos_cloud`, which
answers with the *built* `index.html` out of `dist/`, whose hashed asset paths do not exist on the
dev server. Result: two 404s and a white screen. Verified fine in production shape — `pos_cloud`
serving its own build resolves `/admins` correctly — so this is a developer-experience defect, but
it means nobody working in `pnpm dev` can see the screen they are editing.

### D5 · An unknown `/admin` path answers `200 text/html`

While probing, `POST /admin/admins/invites` (a wrong guess at the route) returned **200** with the
SPA's `index.html` rather than a `404` carrying the AIP-193 envelope the rest of `/admin` is careful
to emit. The SPA fallback is catching unmatched `/admin/*` paths. Every client typo therefore
arrives as HTML that fails to parse, instead of a structured `NOT_FOUND`. Small, but it undoes the
error-envelope work for the one case where the caller has the path wrong.

## 3w.2 Performance — measured, and one clear win

The owner's instruction was that optimisation stays in scope regardless of my argument that no
problem had been measured. So it was measured, in the real deployment shape (`pos_cloud` serving its
own embedded build and the API on one origin).

| Measurement | Result |
|---|---|
| Cold load to network idle (loopback) | 596 ms |
| First contentful paint | 76 ms |
| DOM content loaded | 62 ms |
| First-visit payload | `ui.js` 270,775 B + `index.js` 48,395 B + `index.css` 25,200 B ≈ **344 kB** |
| Per-route chunk | 471 B – 46,286 B, fetched on demand (code-splitting works) |
| Requests per screen | 2 – 9, all `200` except the two config `404`s below |
| `content-encoding` on every asset | **none** |
| `cache-control` on hashed assets | `public, max-age=31536000, immutable` ✓ |

The front-end is not the problem: the paint is fast, the split is real, the caching is right. **The
problem is that `pos_cloud` serves the bundle uncompressed.** 344 kB of highly compressible JS and
CSS goes over the wire at full size where gzip would send roughly 80 kB. On loopback that costs
nothing, which is why it has never been noticed; on a store manager's 4G tether or a provincial
ADSL line it is the difference between a console that opens in under a second and one that takes
three or four. `docs/cloud-admin-ux-plan.md` §3.6 already flagged "no compression … layers in the
binary" from the code audit; this confirms it against the running server and quantifies it.

Two secondary observations. `GET /admin/stores/{id}/config` and `.../config/versions` answer `404`
for a store that has never had a configuration published — which is every newly provisioned store —
so the Configuration screen's first load always includes two failed requests and two red lines in
the browser console. An empty tree is a legitimate state and should read as `200` with an empty
document, not as a not-found. And the Catalog screen fires **nine** requests on open (five chunks,
four collections); the collections are independent and correctly parallel, but on a real catalogue
they will each be an unbounded list.

## 3w.3 What the journey looks like, screen by screen

Beyond the defects, the walk surfaced a consistent set of design gaps. These are the "international
standard" half of the brief, and they cluster into six.

**1. There is no get-started.** This is the gap the owner named directly. A freshly enrolled admin
lands on a 1440-px canvas holding one card that says *"Choose the store in the top bar to continue."*
No numbered path, no "create your first tenant", no link to the wizard, no sense of how many steps
remain. The information needed to build it already exists — the console knows there are zero
tenants, zero stores, zero published configurations and zero paired devices — it is simply never
assembled into a checklist. And at the other end of the path, the wizard's final step names the next
two mandatory actions in a green banner — *"Next: activate the store's devices (Activation) and
publish its configuration (Configuration)"* — **as plain text, not as links**, leaving the operator
to find two entries in a thirty-item nav.

**2. The navigation is a thirty-item wall.** Six groups, all expanded, all text, no icons, no
collapse, no persistence of what is open. On desktop it is 1,609 px tall — which is why *every one
of the thirty screens reports a page height of 1,609 px regardless of its content*: the sidebar,
not the content, sets the length of the page. On a phone it degrades into a wrapped word-soup of
thirty links that the operator scrolls past — about 700 px of it — before reaching content, on every
screen. No horizontal overflow anywhere (checked at 390 px and 820 px), so it is not broken; it is
just not usable there.

**3. The nav's readiness dot means the opposite of what it looks like.** `contextReady(scope)`
renders a filled `bg-accent` dot when a screen's context is satisfied and a hollow one when it is
not. `bg-accent` is the brand red. So a console that is *working correctly* shows twenty small red
dots down the left edge, and a screen that is *blocked* shows nothing at all. The logic is right; the
signal is inverted against every convention a user brings with them, where a red dot in a sidebar
means unread, attention, or error.

**4. A brand-new store reads as a disaster.** The first screen after provisioning shows three
headline tiles: **"Not reporting"**, **"Behind"**, **"Nothing yet"** — the first two in large red
type. All three are accurate and all three are the *expected* state of a store whose box has not
been installed yet. There is no "provisioned, awaiting installation" posture, so the console's
opening impression of a successful setup is alarm. The six tiles are also three different visual
treatments of a number — huge display type, small bold, and an em dash — because there is no
KPI/metric primitive to be consistent with.

**5. The component kit is bypassed exactly where it matters.** `kit.tsx` exports `EmptyState`,
`Modal`, `Drawer` and `ConfirmDialog`. The Stores screen — the first master-data screen on the
mandatory path — uses none of them: its two empty states are bare sentences ("No stores yet. Create
one below.", "No brands yet"), and its create forms are two more stacked cards *below* the empty
tables, so the operator scrolls past two "nothing here" messages to reach the thing that fixes it.
Reports stacks six cards of which five are empty, each with its own differently-worded emptiness.
Six primitives that a standard admin dashboard has are absent from the kit entirely: **Tooltip,
Skeleton, KPI/Metric tile, Bulk actions, Kebab/overflow menu, and Tabs** (the wizard's three steps
and Catalog's sub-navigation are two separate hand-rolled implementations of the same thing).

**6. Small consistency debts that add up.** The language switcher is an unstyled native `<select>`
in a header of otherwise custom controls. There is no user identity anywhere in the header — no
name, no email, no role — so an admin cannot see who they are signed in as, which will matter the
moment D1 is fixed and there is more than one. On the Stores screen a disabled "Save" renders as a
washed-out red that reads as broken, immediately beside "Archive" — the screen's most destructive
action — rendered as a solid brand-red primary. Reports' date fields are native `type="date"`, so
they render `mm/dd/yyyy` to a Vietnamese operator and offer no Today / This week / Last 30 days
presets. The wizard's handoff step prints the store's live secret as page text with a copy button on
one file and none on the other, repeats the same red warning banner twice, and puts "Finish" 2,300
px down the page with no sticky action bar. Raw ULIDs are correctly hidden behind "Technical
details" on the Stores screen and shown bare in the context picker and on the wizard's final step.

## 3w.4 The plan, ordered mandatory → optional

Ordered the way the owner asked: top to bottom, each stage a precondition of the next, along the
real path from an empty cloud to a shop that trades. Nothing below is a big-bang redesign; the
sequence is deliberately "make the mandatory path correct and legible, then raise the whole surface,
then style it".

### Stage 0 — the harness (one PR, precondition for everything after)

The console has **no test runner and no test dependency of any kind**; `pnpm build` is
`tsc --noEmit` plus four lint scripts. That is why the four defects above survived F0, F1, F2, F3,
G1, G2 and every track since, and it is why a redesign that touches 24,000 lines is currently
un-checkable. Stage 0 adds Vitest plus a browser driver and about a dozen behavioural tests that
pin the mandatory path: sign in, create a tenant and assert the URL carries it, follow a link naming
a second store and assert the header names *that* store, open a confirm dialog and assert the typed
name guard resets. Each of D1–D4 gets the test that would have caught it. See §3w.5 for why this is
first and not last.

### Stage 1 — the mandatory path is correct (defects)

1. **D1 — per-admin login.** The largest piece of work in the plan and the only one that needs an
   ADR: `LoginRequest` gains an identity, `login` resolves the admin by email and checks *that*
   admin's `password_phc` and `totp_secret`, the session binds to them, and the enrolment URI is
   labelled with the invitee's own email. Until this lands, Track G is decoration.
2. **D2 — one place resolves a name from an id.** `setStoreId`/`setTenantId` either clear the
   remembered name or resolve the real one from the registry. Clearing is the safe half and is a
   two-line change; resolving is the right answer and needs a small lookup the picker already has.
3. **D3 — `createTenant` calls `goToContext`.** One line.
4. **D4 — the dev proxy stops swallowing `/admins`.** Match `^/admin/` rather than the `/admin`
   prefix. One line.
5. **D5 — unmatched `/admin/*` answers a `404` envelope**, not the SPA.
6. **The empty config tree answers `200`**, not `404`, so a new store's Configuration screen opens
   clean.

### Stage 2 — the operator can find their way (the get-started)

7. **A get-started checklist on the landing screen**, assembled from state the console already
   fetches: tenant → brand → store → API key → installer downloaded → device paired → configuration
   published, each row showing done/next/blocked and linking straight to the screen that does it.
   This is the "admin phải có get-start setup theo tenant, brand, store, file cài" the owner asked
   for, and it is the single highest-value screen in the plan.
8. **Make the wizard's closing banner into links** — Activation and Configuration, by name, one
   click.
9. **Collapse the navigation**: groups that remember their state, icons, a persistent active trail,
   and a drawer under `md`. Fix the readiness dot's semantics while it is open — a neutral or muted
   marker for ready, and something visible for blocked, which is the case that actually needs the
   operator's attention.
10. **A "provisioned, not yet installed" posture** on the store overview, so a successful setup does
    not open in three red alarms, with the alarm tiles reserved for a store that *was* reporting and
    stopped.

### Stage 3 — performance, where it is measured

11. **Compress the served bundle** (`tower-http` `CompressionLayer`, or the compression Caddy is
    already in front of): ~344 kB → ~80 kB on a first visit. One layer, the largest single win in
    the whole plan, and the answer to "vẫn phải tiếp tục tối ưu" that is backed by a number.
12. **Skeletons instead of the bare `common.loading` string** in the sixteen places that show it —
    perceived performance, and the missing primitive from §3w.3.
13. **Seed a volume fixture and measure the tables**, then decide on virtualisation. Right now every
    list has been seen only empty; committing to virtualisation before measuring would be guessing.

### Stage 4 — the missing primitives (raises every screen at once)

14. **Tooltip, Skeleton, KPI tile, Bulk actions, Kebab menu, Tabs** into `kit.tsx`, then adopt them
    where the hand-rolled version exists — Catalog's tabs and the wizard's steps onto one `Tabs`,
    the store overview's six tiles onto one `KpiTile`, the two hand-written empty sentences on
    Stores onto `EmptyState`.
15. **Create actions move into the page header** as a primary button opening a `Modal`/`Drawer`,
    instead of a form card below the empty table. Stores first, since it is on the mandatory path.

### Stage 5 — state management

16. The console holds **400+ `createSignal`** against **9** `createStore`/`createResource`: there is
    effectively no data-fetching abstraction, so every screen hand-rolls load/busy/error/refetch and
    each one gets to be subtly wrong in its own way. `Menus.tsx` alone has 35 signals. A single
    resource helper — one place that owns loading, error, refetch-after-mutation and the ETag — is
    the structural fix, migrated screen by screen behind the Stage 0 tests. This is where the brief's
    "chuẩn hoá state management" belongs, and it is deliberately *after* the primitives, because a
    resource helper's shape is easier to get right once the components that consume it are settled.

### Stage 6 — optional: the visual system

17. Typography scale, spacing rhythm, elevation, iconography, motion, dark mode, and the
    header/identity/avatar work. Real value, no operational risk, and cheapest to do last — once the
    primitives exist, restyling is a token pass rather than forty screen rewrites.

## 3w.5 Why the harness is Stage 0

The owner asked what a test harness is. The plain answer, in the terms of this console:

Today, the only thing that checks the dashboard before it ships is the TypeScript compiler and four
lint scripts. They can tell you a name is misspelled or a translation key is missing. They cannot
tell you that clicking "Create" leaves the address bar behind, that a store link shows the wrong
shop's name, or that an invited admin can never sign in — because none of those is a type error.
They are all *behaviour*, and nothing in the repository runs the behaviour.

A harness is two things. First a **test runner** — for this stack, Vitest, which the project can add
in an afternoon — that runs small programs on every build. Second, the **tests**: each one renders a
piece of the console, does what an operator does, and asserts what should happen. The test that
would have caught D2 is roughly this long:

```
render the console with store A picked
navigate to a link naming store B
expect the header to read "store B"
```

Three lines of intent, maybe fifteen of code. Run in under a second, on every commit, forever.

That is the whole argument for putting it first. Four of the five defects in §3w.1 passed through
seven completed tracks and dozens of reviewed pull requests without anyone noticing, because the
only way to notice was for a human to click exactly the right sequence. Stages 1 through 6 will
touch nearly every one of the forty screens; without a harness, each of those changes is another
chance to introduce a defect that will again be found only by accident, months later, possibly by a
franchisee. With one, the mandatory path is checked automatically every time anyone touches
anything — which is also the only honest way to reach the brief's "hoàn toàn không còn lỗi
logic/giao diện". A claim of no bugs is not something a redesign can deliver; it is something a
harness lets you keep checking.

It is not free, and it is not a redesign. It buys nothing visible to an operator on the day it
lands. It is the difference between the next six stages being safe and being a gamble.

## Correction — the compression finding was measured on the wrong path (2026-09-09)

§3w.2 above reports that `pos_cloud` serves the SPA bundle with no `content-encoding`, calls that
the largest measured performance win in the plan, and §3w.4 item 11 schedules a compression layer to
take it. **The measurement is real and the conclusion is wrong.** The correction, on picking the work
up:

`deploy/Caddyfile.d/site.caddy` line 8 is `encode zstd gzip`, and that file is imported from inside
the site block of **all four** TLS postures — `acme-dns01`, `acme-http01`, `byo-cert` and `external`
(ADR-0090). Every deployed `pos_cloud` sits behind a Caddy that already compresses, and with zstd
preferred over gzip, which is better than what item 11 proposed.

What §3w.2 actually measured was `pos_cloud` on `127.0.0.1:8080` — the loopback address of the
process *behind* the ingress, which no operator's browser ever talks to. The bytes on the wire in
production were already ~80 kB, not 344 kB. There was no win to take.

So item 11 is withdrawn rather than built. Adding `tower-http`'s compression feature would pull new
transitive crates in to duplicate what the ingress does, and AGENTS.md §"never" requires an ADR
merged first for a dependency change — an ADR whose Consequences section would have to admit the
feature is redundant in every supported deployment.

Two things this leaves behind. First, the honest state of the performance question: after
measurement, **the console has no demonstrated performance problem in the path an operator uses** —
76 ms to first contentful paint, real per-route code-splitting, correct immutable caching, and
compression at the ingress. What survives of Stage 3 is item 12 (skeletons, which are *perceived*
performance and a missing primitive) and item 13 (seed a volume fixture and measure the tables,
which is still unmeasured and still the one place a real problem could be hiding). Second, a lesson
worth writing down because it cost a wrong headline in a plan that had just been merged: a
measurement taken against a process is not a measurement of the product when the deployment puts
something in front of it. The topology is part of the system under test.

The one deployment this would matter for is `pos_cloud` run bare with no proxy in front. `deploy/`
does not describe that posture and `docs/deploy-runbook.md` does not support it, so it is not a case
the plan needs to serve.

## Stage 3 delivered — the table volume question, answered with numbers (2026-09-09)

§3w.4 item 13 said "seed a volume fixture and measure the tables, then decide on virtualisation.
Right now every list has been seen only empty; committing to virtualisation before measuring would
be guessing." The previous correction narrowed Stage 3 to items 12 and 13 and called item 13 "the
one place a real problem could be hiding". This is the measurement, and it closes the question.

### How it was measured

`dashboard/bench/volume.html` mounts the real `DataTable` with N synthetic five-column rows — an
id, a name, a status, a number and a date, the shape the Stores, Devices, Employees and Items
screens actually render — and reports the time from before `render` to after the browser has
painted, two `requestAnimationFrame` ticks later. `dashboard/bench/measure.mjs` drives it in
Chromium against the dev server and prints the table below. Both are committed: the numbers belong
in the repository rather than in a terminal that has since closed.

| rows | rendered | paint |
| --- | --- | --- |
| 100 | 100 | 36 ms |
| 500 | 500 | 81 ms |
| 1,000 | 1,000 | 161 ms |
| 2,000 | 2,000 | 322 ms |
| 5,000 | 5,000 | 1,614 ms |
| 10,000 | 10,000 | 3,988 ms |
| **10,000, `pageSize` 25** | **25** | **37 ms** |

Linear to about 2,000 rows, then super-linear: the cost past that is the layout and paint of a very
tall table, not Solid's per-row work. And the last row is the finding — **a page size makes the row
count irrelevant**. Ten thousand rows behind a page of 25 paints in the same time as a hundred rows
unpaged.

### The decision: virtualisation is not built

It would be a large, risky change to the one component every list screen renders, in order to solve
a problem `pageSize` already solves at a hundred times the expected load. `DataTable`'s claim that
it is "right-sized for tens to hundreds of rows" turns out to be true, and now measured rather than
asserted.

What the measurement *did* find is a different, smaller problem it was not looking for. Of the 38
tables in the console, **eight rendered every row they were given** — no server paging and no client
page size. Not because anyone chose that: `pageSize` is optional and easy to forget. The eight were
the floor's QR tokens, the two Layout taxonomy tables, my own sessions, People's assignments,
Stations' routing rules, and — the one that can genuinely reach thousands — a menu's sections and
placements, which scale with the item catalogue.

All eight now take `CLIENT_PAGE_SIZE` (25, matching what the server-paged screens already ask for).
This is invisible at the volumes those lists actually hold, because the pager renders only when the
set exceeds the page: a table of three routing rules looks exactly as it did. It is a ceiling, not a
redesign. `dashboard/tests/table-volume.test.ts` fails if a table appears with neither a client page
size nor server paging, because the next screen will forget it too.

### Item 12, and a count that was wrong

The skeletons shipped: a `Skeleton` primitive in `components/ui.tsx` and thirteen adopting call
sites. The accessibility half is the half that mattered — replacing `<p>Loading…</p>` with grey bars
would have made every loading state *silent* for a screen reader, a regression dressed as an
improvement, so the container is a `status` region carrying the same words and the bars are
`aria-hidden`.

Item 12 says "the sixteen places that show it". The real number is **fourteen call sites**; sixteen
was a count of `grep` hits, which included the two entries in `en.json` and `vi.json`. Of the
fourteen, thirteen became skeletons and one stayed as words on purpose — the context picker's create
button, whose own label reads "Loading…" while a request is in flight. A button is not a placeholder
for content that is arriving, so a skeleton there would have been wrong rather than missing.

## Stage 6 — the visual system, part one: what the tokens said and what the code did (2026-09-09)

§3w.4 item 17 lists Stage 6 as "typography scale, spacing rhythm, elevation, iconography, motion,
dark mode, and the header/identity/avatar work", and calls it optional, no operational risk, and
cheapest to do last. That framing was right about the ordering and wrong about the risk: measuring
the six subjects before restyling anything found that two of them were not missing polish but
**broken**, and one of the two only in dark mode, which is why nobody had reported it.

### What was measured, and what it found

Six greps, before any change:

| subject | state found |
| --- | --- |
| typography | **clean.** 502 size classes, every one on the six-step token scale, no arbitrary values |
| colour | **clean.** No hex, `rgb()` or raw `oklch()` anywhere in `src/` — every colour goes through a token |
| spacing | **clean.** No arbitrary spacing values |
| elevation | **no token at all.** 6 sites on Tailwind's default `shadow-lg` |
| motion | **1 site in the whole console**, and it bypassed the token it was built from |
| iconography | absent from the nav (still Stage 6, still outstanding) |

So four of the six subjects the plan lists as Stage 6 work were already done, which is the answer to
whether a "token pass" was needed: the tokens were being honoured everywhere they existed. The two
findings are the two places a token did *not* exist, and both had produced a real defect.

### Elevation: the same class name was correct in one theme and inert in the other

`shadow-lg` is `0 10px 15px -3px rgb(0 0 0 / 0.1)`. Over the light palette, that is a shadow. Over
the dark palette — `--canvas` at `oklch(0.17 0.01 260)` — a ten-percent black shadow on a near-black
ground is nothing at all. Every floating surface in the console (both modal shapes, the command
palette, the org-switcher dropdown, the notification dropdown, the toast) therefore had depth in
light mode and none in dark, reading as a flat patch of slightly different grey held apart from the
page by its 1px border alone.

This is precisely the failure the colour tokens exist to prevent, and elevation was the one visual
property still outside them — because a drop shadow does not look like a colour. It is one:
`--elevation-raised` and `--elevation-overlay` are now runtime variables with a value per theme,
forwarded as the `shadow-raised` and `shadow-overlay` utilities. Dark does not merely deepen the
alpha (though it does, from 10% to 50–60%, which is where a shadow starts to register on that
ground); the overlay step also carries a hairline light ring, because on a dark background the eye
reads the *lit top edge* of a floating surface as "above", and a shadow cannot draw an edge that
faces the light.

`Card` gained `shadow-raised`, which it never had. In the light palette `--surface` and `--canvas`
differ by two percent of lightness, so until now every card in the console was separated from the
page by its border and nothing else.

### Motion: a token with zero consumers

`--ease-token` had been declared in `@theme` since P6. Nothing used it. The one component in the
console that animated anything — `Button` — wrote `ease-[cubic-bezier(0.2,0,0,1)]`, the token's
exact value, longhand as an arbitrary Tailwind class, next to a bare `duration-150`. The token
existed, its value was duplicated in a class string, and `tokens.css`'s own opening promise that "a
utility resolves to a token, never a magic number" was not true of the only motion in the product.

The fix is not to write `ease-token` at that one site. It is to set Tailwind's two
`--default-transition-*` variables from the tokens, so that a **bare** `transition-colors` picks up
the house easing and duration and a site that wants the house motion writes no number at all. That
is the only version of a motion token a screen cannot drift away from. Sixteen hover states that
changed colour instantly — the nav entries, the group headings, the catalog tabs, the dropdown rows,
the dialog close buttons, the pager arrows — now transition, and not one of them names a duration.

One hover was deliberately left alone: `Layout.tsx` hovers to an underline, and a text-decoration
appearing has nothing to interpolate, so a transition class there would read as motion and produce
none.

### A third finding, in the gate rather than the product

`tokens.css` holds three blocks that carry runtime values: light, system-dark
(`@media (prefers-color-scheme: dark)`) and chosen-dark (`:root[data-theme="dark"]`). The WCAG-AA
contrast gate — which runs on every build in both front-ends — read **two** of them: light and
chosen-dark. The one it skipped is the palette a viewer whose system is dark and who has never
picked a theme actually gets, which is to say the default dark experience was the only palette never
audited. It passed only because it duplicates the chosen-dark block verbatim, and nothing checked
that it still did; editing one and not the other would have shipped an unaudited palette with no
failure anywhere.

`scripts/wcag-contrast.mjs` now audits all three (36 gated pairs, up from 24) and, before that,
checks the blocks against each other: the same token names in all three — the invariant the file
states in prose and nothing enforced — and identical values in the two dark blocks, which is what
makes duplicating them safe. The till's copy of the gate had the identical hole and got the identical
fix; the two scripts remain byte-identical.

### And a rule that overruled a judgement call

The two scripts are byte-identical because I kept them so by instinct. It turns out the tree requires
it: `xtask mirrored-files` declares four pairs that must match byte for byte across the two front-end
build roots — both `tokens.css`, both contrast gates, and the two i18n gates — because the packages
have separate Vite builds and cannot share a module, so the substitute for a shared module is a gate
that fails the moment a copy drifts.

Which means the reasoning above, that the till "was not given elevation tokens it has no use for",
was wrong, and CI said so: the token set is one file with two homes, not two files that happen to
agree. So `ui/src/styles/tokens.css` carries the elevation and motion tokens too. The till draws no
shadow and runs no transition today, so nothing about it changes except 1.1 kB of unused custom
properties in its stylesheet — and when a kitchen screen does need to lift a panel off a near-black
background, the token is already there and already correct for that palette.

Worth recording as a lesson rather than a footnote: "the till doesn't need this" was a reasonable
judgement about the till and the wrong judgement about the token set, and the thing that knew better
was a gate written by whoever last got this wrong.

### Where the checks live, and why they are split

`dashboard/tests/visual-tokens.test.ts` owns the component half — no `.tsx` reaches past the tokens
to Tailwind's default shadow scale, none writes its own easing or duration, every colour hover
transitions (checked per class value, not per file: a file-level check passes a file whose *second*
hover site was missed, which is the shape this defect actually had), and both elevation steps have
consumers, so the suite cannot pass by there being no shadows left to draw.

The CSS half is in the contrast gate rather than beside it, and not by preference. Vitest stubs CSS
imports (`css: false`), so a `?raw` import of a stylesheet resolves to an **empty string** under the
test runner and every assertion about its contents passes vacuously — which the first version of
this suite did, silently, until the probe that caught it. A check that cannot see its subject is
worse than no check. The gate already parses `tokens.css` and already exits non-zero, so the
invariant went where the file can actually be read.

Each of the five component checks and both new gate checks was verified by breaking it and watching
it fail while naming the offending file, then restoring the file byte-identically.

### Still outstanding in Stage 6

Iconography (the nav icons deferred from #261), the theme choice the console cannot offer — the
`data-theme` attribute is honoured by the CSS and nothing in the console ever sets it, while the
till has a toggle for it — and the header identity block, which is the "header/identity/avatar" half
of item 17. All three are additive.
