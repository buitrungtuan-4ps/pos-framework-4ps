# ADR-0043 — The translation grid: one jsonb per tenant, `en` required as the fallback

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0020](0020-i18n-runtime.md) · [ADR-0033](0033-config-tree.md) · [ADR-0039](0039-config-delivery.md) · `docs/roadmap.md` P7

**Context.** `docs/roadmap.md` P7 requires a **translation grid** — the cloud-side place a tenant
authors the localized strings for its menu and content, feeding the edge's ICU i18n runtime. [ADR-0020](0020-i18n-runtime.md)
already fixed the runtime rule that matters here: **`en` is the always-present fallback**, so a
missing locale degrades to English rather than to a blank. The grid must make that rule structural,
and it must have a storage shape and an authoring surface.

**Decision.**

- **The grid is `key → { locale → string }`, one jsonb document per tenant.** A translation key (a
  `snake_case`/dotted content key like `menu.item.pho`) maps to a small map of locale → rendered
  string. The whole tenant grid is one `jsonb` row keyed and RLS-isolated by tenant, exactly the shape
  the config tree ([ADR-0033](0033-config-tree.md)) and rollups use — authored and read whole, cheap
  to fetch, and forward-compatible (an edge on an older build keeps locales it does not recognize).

- **Every key must carry `en`; a grid that violates it is rejected.** The always-present-fallback rule
  ([ADR-0020](0020-i18n-runtime.md)) is enforced at write time: publishing a grid whose every key does
  not have a non-empty `en` value is a `422` carrying the offending keys, and nothing is stored — so
  the edge can *always* fall back to English, structurally, rather than by hope. Validation is a pure
  function over the grid, tested without a database.

- **Authoring is a super-admin `PUT`/`GET` of the whole grid.** `GET /admin/translations?tenant_id=…`
  returns the tenant's grid (empty if none yet); `PUT /admin/translations?tenant_id=…` replaces it,
  validated. Whole-grid replace, not per-cell patch: the grid is small, an admin edits it as a unit,
  and replace-with-validate keeps the `en` invariant checkable in one place. The routes are a merged
  sub-router with their own `{translations, admin, clock}` state (like reconciliation and device
  onboarding), so the grid adds **no `CloudApp` generic**.

**Rejected.**

- **A row per `(key, locale)`** — rejected: it scatters one logical grid across many rows, makes the
  "every key has `en`" check a query rather than a pure function, and buys nothing for a
  tenant-sized grid the edge fetches whole.
- **Per-cell PATCH authoring** — rejected for this slice: it multiplies routes and makes the `en`
  invariant a cross-request property (delete the last `en` cell and the grid is now invalid). A
  validated whole-grid replace keeps the invariant atomic. A cell editor can layer on later over the
  same store.
- **No `en` requirement** (accept any grid, fall back at read time to the key itself) — rejected: it
  defeats [ADR-0020](0020-i18n-runtime.md)'s guarantee. Showing a raw key like `menu.item.pho` to a
  guest is the exact failure the fallback exists to prevent; enforcing `en` at authoring time is where
  it belongs.

**Consequences.**

- `store-postgres` migration `0008` adds a `translations` table (tenant_id PK, `grid` jsonb),
  RLS-isolated by tenant, behind a new `TranslationStore` seam and the `persistence.rs` bridge; a fake
  backs the route tests. No new dependency.
- The `en`-fallback invariant is a pure `validate` on the grid, unit-tested (a grid missing `en` on a
  key is rejected with that key named), and the admin round-trip (`PUT` then `GET`, and the `422` on an
  invalid grid) is proven over the fakes.
- **The store-facing fetch is the next slice.** This lands authoring and storage — the grid a tenant
  fills in. The store-facing `/sync` read that hands the edge its grid (a `read_config`-adjacent pull,
  the same split config delivery drew, [ADR-0039](0039-config-delivery.md)) and the `pos_edge` loader
  build on this store.
