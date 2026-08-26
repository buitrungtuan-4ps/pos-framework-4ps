# Cloud admin dashboard — UX & master-data management plan

**Status** Proposed · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-26

The cloud dashboard grew screen-by-screen alongside the runtime it configures. Each screen works,
but together they do not yet read as one **admin console**: entities are scattered across a flat menu,
the working context is a hidden per-browser toggle that fails with a raw backend error when unset, and
some screens still expose raw ULIDs and raw JSON. This plan re-frames the whole surface around a
single idea — **everything the fleet is configured with is master data, managed the same way** — and
sets out a phased path to a framework-standard, multi-country admin console.

It is a companion to `docs/roadmap.md` (which took the *runtime* to pilot-ready) and to
`docs/ui-ux.md` (the operator-facing design principles, which this reuses). It changes no runtime
behaviour on its own; each phase lands as its own reviewed PR, with an ADR where it moves a contract.

---

## 1. The problem, concretely

**Symptom the user hit:** `tenant_id or the store id is not a ULID` (HTTP 400).

**Root cause:** the dashboard's working context (`tenantId`/`storeId`) is remembered per-browser in
`localStorage` and **defaults to an empty string**. Several screens issue a store-scoped request on
mount; before the operator has picked a store in the context picker (or in a fresh browser), the
request carries `""`, which the server correctly rejects as a non-ULID. `Config.tsx` already guards
with `when={tenantId() && storeId()}`, but the guard is applied inconsistently across the other ten
context-reading screens. So the same class of failure surfaces as a cryptic backend error rather than
"pick a store first".

**The deeper problem it points to.** The 400 is a symptom of four structural gaps:

1. **No global context contract.** Context is optional, hidden, and unguarded, so screens fail
   instead of guiding.
2. **No master-data model.** Only Tenant/Brand/Store/Device and Catalog are managed entities.
   Employees, roles/permissions, payment methods, channels, countries/locales, tax and suppliers are
   either edited as raw config/JSON or not manageable at all.
3. **No standard screen pattern.** There is no shared List → Detail → Edit CRUD pattern, no
   breadcrumbs, no search/filter/sort/bulk, no consistent empty/loading/error states.
4. **Raw internals leak.** `Config` is a raw-JSON textarea; ULIDs still surface in places; validation
   errors are backend strings, not field-level messages.

---

## 2. Where we are today (audit)

| Area | Today | Gap |
|---|---|---|
| **Navigation** | 12 flat routes (Reports, Stores, Catalog, Layout, Config, API keys, Devices, Webhooks, Translations, Activation, New store, Login/Setup) | No grouping, no breadcrumbs, no scope cues (which sections are global vs tenant- vs store-scoped) |
| **Context** | `ContextPicker` (ADR-0065) picks tenant→store by name; stored in `localStorage`, default empty | Optional and unguarded; no "you must choose a store" gate; not reflected in the URL, so a screen can't be linked or reloaded into a context |
| **Master data present** | Organization tree (Tenant/Brand/Store/Device); Catalog (items, menus, modifiers, categories, sections, tax classes, display taxonomy, layouts) | — |
| **Master data missing** | — | **Employees & roles/permissions** (the registry exists in `pos-core` but has no admin UI), **payment methods**, **sales channels**, **countries/locales/currencies** (locale packs exist in `pos-country` but aren't managed), **suppliers/inventory master**, first-class **tax-rate tables per channel/country** |
| **Entity screens** | Bespoke per screen; `Stores` and `Catalog` are the most complete; others are thin | No shared CRUD kit; create/edit/archive/audit patterns differ per screen |
| **Config** | Raw JSON textarea per level + effective-config viewer | No form-driven capability/flag editing; no diff/preview; validation is a backend error list |
| **Design system** | `dashboard/src/components/ui.tsx` (Card, PageHeader, Button, TextField, TextArea, Banner) | No DataTable, EntityPicker, DetailLayout, StatusBadge, EmptyState, ConfirmDialog, breadcrumb, tabs |
| **i18n / multi-country** | ICU i18n runtime + translation grid; `en` fallback | Country/locale/currency/tax not surfaced as managed data; no per-tenant branding; no per-country default sets |
| **Cross-linking** | Screens are islands | A store doesn't link to its devices → its config → its published menu; no "related entities" |

---

## 3. The target: one master-data console

### 3.1 Principle

Everything the fleet runs on is **master data** — authoritative reference entities, managed centrally
in the cloud, versioned, validated, and delivered to stores through the existing config tree and
catalog publish paths. Operational data (orders, shifts, rollups, telemetry) is **not** master data:
it is read-only reporting, kept in a separate part of the console.

### 3.2 Master-data domains

| Domain | Entities | Scope | Delivery to edge | Classification |
|---|---|---|---|---|
| **Organization** | Tenant, Brand, Store, Device | global → store | registry + activation | T3 internal |
| **Catalog & Pricing** | Item, Menu (inheritance), Modifier group, Category, Section, Tax class, Display taxonomy, Layout | tenant → store | catalog compile → `menu`/`layout` nodes | **T2** (prices) |
| **People & Access** | Employee, Role template, Permission set, PIN policy | tenant → store | synced PIN hashes + role config | **T1** (employee PII) |
| **Localization** | Country, Locale pack, Currency, Channel-keyed tax-rate table, Receipt template | global → tenant | locale pack + config nodes | T3 |
| **Channels & Payments** | Sales channel, Payment method, Terminal binding | tenant → store | config nodes | T3 (terminal creds server-side) |
| **Integrations** | API key, Webhook endpoint, Marketplace/ERP/shipping binding | tenant | provisioned server-side | T2 (vendor terms) |
| **Delivery/Config** | Effective config tree, capability flags, OTA rings | all levels | config tree (existing) | T3 |
| **Operations (read-only)** | Orders, Shifts, Rollups, Reconciliation, Telemetry | store | — (reporting) | T1/T2 mix — anonymize |

The rows already built are Organization and Catalog & Pricing. The rest is the growth path.

### 3.3 Every entity, managed the same way

A single, repeatable pattern (the "framework-standard" the user asked for):

- **List** — searchable, filterable, sortable table; status column (Active/Draft/Archived via the
  existing `EntityStatus`); bulk actions; "New" affordance; empty state that explains and offers the
  first action.
- **Detail** — header with name + status + key facts; tabs for related entities (a Store → Devices,
  Config, Menu, Staff); an **audit trail** (who changed what, when); actions (edit, archive, publish).
- **Edit** — validated forms with **field-level errors** (never a raw backend string), no raw JSON,
  optimistic save with confirmation; destructive actions behind a confirm dialog.
- **Cross-links** everywhere — an id is always rendered as a name that links to that entity's Detail;
  raw ULIDs appear only in a muted "technical details" area.

---

## 4. Navigation & context model (kills the ULID error class)

### 4.1 Grouped, scope-aware navigation

Replace the flat 12-item menu with grouped sections, each tagged by the scope it needs:

```
Overview            (global)     — fleet health, recent activity
Organization        (global)     — Tenants, Brands, Stores, Devices
Catalog & Pricing   (tenant)     — Items, Menus, Modifiers, Categories, Tax classes, Layouts
People & Access     (tenant)     — Employees, Roles, PIN policy
Localization        (global)     — Countries, Locales, Currencies, Tax tables, Receipt templates
Channels & Payments (tenant)     — Sales channels, Payment methods, Terminals
Integrations        (tenant)     — API keys, Webhooks, Marketplaces
Config & Rollout    (store)      — Effective config, Capabilities, OTA rings
Operations          (store)      — Reports, Reconciliation, Activation
System              (global)     — Translations, Audit log, Settings
```

### 4.2 The context contract

- A section declares its scope: **global**, **tenant**, or **store**.
- An **org switcher** in the top bar sets tenant → store, reflected in the **URL** (e.g.
  `/t/:tenant/stores/:store/config`) so a screen is linkable, reloadable, and shareable.
- A **context gate**: entering a tenant- or store-scoped section without the required context does
  **not** fire a request — it shows a friendly "Choose a {tenant|store} to continue" panel that opens
  the switcher. This removes the entire `… is not a ULID` failure class at the UX layer (the server
  guard stays as defence in depth).
- **Breadcrumbs** show the path (Tenant › Store › Config) and are themselves the quickest switcher.

---

## 5. Multi-country / multi-tenant as first-class

A framework meant for many countries must manage the country differences as data, not code:

- **Countries & locales** become managed master data: currency, timezone, date/number formats,
  channel-keyed tax rates, receipt template, fiscal profile (for the future `fiscal-vn` and its
  siblings). New-country onboarding is a data task, not a deploy.
- **Per-tenant branding** — name, logo, colours (within the token system) — so the console and the
  store UI can carry the brand.
- **i18n throughout** — the ICU runtime already exists; every new screen routes strings through it,
  and layouts must survive text 30% longer than English and RTL.
- **Currency & timezone awareness** — money always shows its currency; times always show the store's
  timezone and business date (the classic rollup-in-server-timezone bug is already handled in
  `pos-core`; the console must not reintroduce it in display).

Data-protection posture (roadmap A6, PDPD): **People & Access** and any operational report handle T1
PII — lawful-basis and retention rules apply, no employee-behaviour monitoring is to be designed, and
raw PII is never shown where an anonymized reference will do.

---

## 6. Design system

Promote the current token kit to a real component library so every screen is built from the same
parts: **DataTable, EntityPicker, DetailLayout (header+tabs), FormField (label+control+error),
StatusBadge, EmptyState, ConfirmDialog, Breadcrumbs, Tabs, Toast**, on top of the existing tokens.

**Decision to confirm (fork A):** whether the edge UI (`ui/`) and the cloud dashboard (`dashboard/`)
consume **one shared component/token package**, or stay two roots kept in sync (today the tokens and
the contrast gate are duplicated and drift-guarded by `xtask mirrored-files`). Recommendation: extract
a shared `web-kit` package once the console kit stabilises (Phase 2), because the two surfaces have
different device targets but the same design language.

---

## 7. Phased roadmap

Sizes relative (S ≈ one PR · M ≈ a few · L ≈ many), dependency-ordered.

| Phase | Scope | Size | Outcome |
|---|---|---|---|
| **A0 — Context gate** | Shared `requireContext` guard + friendly "choose a store" empty state applied to all scoped screens; consistent empty/loading/error states | **S** | The `… is not a ULID` error class is gone; no screen fires an empty-id request |
| **A1 — Console shell** | Grouped scope-aware nav, URL-encoded context (`/t/:tenant/…`), breadcrumbs, org switcher upgrade | **M** | One coherent frame; every screen linkable and reloadable |
| **A2 — Entity CRUD kit** | DataTable/DetailLayout/FormField/StatusBadge/EmptyState/ConfirmDialog; migrate Stores, Devices, Catalog onto it | **M** | One repeatable List→Detail→Edit pattern; field-level validation |
| **A3 — People & Access** | Employee + Role/Permission master data (UI over the `pos-core` permission registry), PIN policy; PDPD-aware | **L** | Staff and roles are managed data, synced to stores |
| **A4 — Localization console** | Countries, Locales, Currencies, channel-keyed Tax tables, Receipt templates as master data | **L** | New-country onboarding is a data task |
| **A5 — Channels & Payments** | Sales channels + payment methods as master data (payment terminals gated on Track A A1) | **M** | Channel/payment config leaves raw JSON |
| **A6 — Config without JSON** | Form-driven capability/flag editor with inter-flag validation surfaced inline, diff/preview before publish, audit trail | **M** | `Config` stops being a raw-JSON textarea |
| **A7 — Operations & reporting** | Reports/reconciliation/telemetry as a distinct read-only area with the console's shell | **M** | Clear master-data vs operational-data split |
| **A8 — Shared web-kit + branding** | Extract the shared component package (fork A); per-tenant branding | **M** | One design system across edge + cloud |

**Cross-cutting:** an **audit log** (who changed which master-data entity, when) added with the CRUD
kit (A2) and shown on every Detail; **RBAC-aware nav** (a console operator sees only what their role
allows) once People & Access lands (A3).

---

## 8. Decisions to confirm

- **Fork A — shared web-kit:** one package for `ui/` + `dashboard/`, or keep two drift-guarded roots?
  (Recommend: extract at A8.)
- **Fork B — master-data scope:** ship all domains in §3.2, or start with People & Access + Localization
  (the two that unlock multi-country) and defer Suppliers/Inventory master to a later track?
  (Recommend: People & Access + Localization first.)
- **Fork C — approach:** incremental refactor screen-by-screen onto the new shell/kit (lower risk,
  the whole thing keeps working throughout), or a parallel rebuild? (Recommend: incremental — A0/A1
  first make everything better immediately without a rewrite.)

---

## 9. Immediate next step

Land **A0 (context gate)** now — it removes the reported error, is small and safe, and every later
phase builds on the same guard. A1 (console shell) follows. Everything after is sequenced by the forks
above once confirmed.
