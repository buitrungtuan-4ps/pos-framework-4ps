# ADR-0098 — Paging is a second read, not a change to the read that exists

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-03
**Relates to** [ADR-0081](0081-reports-and-analytics.md) (the windowed `?from=&to=&limit=` reads this
generalises) · [ADR-0069](0069-audit-trail.md) (the one seam that already takes a filter struct) ·
[ADR-0095](0095-conditional-writes-for-collections.md) (the per-row `etag` a paged row still carries) ·
[ADR-0082](0082-catalog-and-layout-rebuild.md) (the `DataTable` that would render the pager) ·
`docs/cloud-admin-ux-plan.md` **F2 Part B, item B3**

**Context.** F2's plan booked one line for this: "`?limit=&offset=&q=&sort=&order=` on the
`admin_list_*` handlers + their store-postgres SQL, returning `{ items, total }`; thread through the
store seams". Read as written, that is a signature change to every list in the cloud and a wire
reshape on every list route. Measured, two of those three clauses are wrong, and the reason they are
wrong is the same reason #152 and slice 5c were: a list route has consumers that are not the table.

The measurement, taken across `crates/pos-cloud/src/http.rs`, the twenty-five cloud seams and
`dashboard/src`:

- **43 `admin_list_*` handlers**, over **45 list-shaped methods on 25 cloud-local seam traits**
  (`CatalogStore` alone declares 11). These are pos-cloud seams, not `pos-ports` ports — no port
  widens here.
- **`GET /admin/items` has six console consumers and only one of them is a table.**
  `catalog/Items.tsx` renders the item master; `catalog/Menus.tsx`, `catalog/Modifiers.tsx`,
  `Inventory.tsx`, `Layout.tsx` and `Stations.tsx` each read the **whole** set to populate a picker or
  resolve an id to a name. `GET /admin/stores` is the same shape: five consumers, one table
  (`Stores.tsx`), while `components/ContextPicker.tsx`, `Devices.tsx`, `Reports.tsx` and
  `Reconcile.tsx` need every store. `GET /admin/tax-classes`: three consumers, one table.
- **Inside `http.rs`, 29 list-call sites across 22 functions are not a list route.** `admin_publish_menu`
  alone makes six (`list_items` → `list_menus` → `list_placements` per menu — the compiler);
  `list_inventory_parts` makes three for the inventory publish; the campaign publish, preview and
  schedule paths make one each; `admin_export_items` and `admin_enable_webhook` one each. And
  **nine of them are `admin_get_*`, `admin_update_*` and `admin_delete_*` on ingredient, recipe and
  supplier**, each listing the whole table to find or snapshot the single row it is about — a read-one
  implemented as a list-all, which is B2's problem and is noted here only because paging the method
  those nine share would break all nine.
- **Only some lists can grow on data.** `vouchers` is the acute one: `MAX_VOUCHER_BATCH` is
  **10 000** per mint and batches accumulate per campaign, so `Campaigns.tsx` fetches every code a
  promotion ever issued — three drops is 30 000 rows in one response.

  **Corrected during B3-1.** This bullet first said the screen "fetches and renders" every code. It
  fetches them; it renders only `existingVouchers().length`, a single number. That makes the case for
  paging stronger rather than weaker — 30 000 rows crossed the wire to produce one integer, and
  `?limit=1` produces the same integer from `total` — but the original sentence was wrong about the
  screen and is corrected here rather than left standing. `media` grows one
  row per uploaded asset, `audit` one per console write, `devices` with the fleet, `employees` with
  headcount, `items` with the chain's menu. Everything else is bounded by how much a human typed.
- **Four lists are closed sets with no table behind them at all** — `permissions`, `capabilities`,
  `countries`, `locales` are compile-time registries. Paging them is ceremony.
- **Five query structs already carry a `limit`** — `AuditReadQuery`, `AlertListQuery`,
  `ReconcileHistoryQuery` and the `/v1` and `/admin` rollup windows — from ADR-0081 and ADR-0069.
  None carries `offset` or `total`, and `ScheduledListQuery` and `OtaRolloutQuery` carry neither
  despite being history reads.

  **Corrected during B3-2.** This bullet is accurate and this ADR then failed to draw its own
  consequence: one of those five reads, `audit`, is in the paged cohort, and on it `?limit=` was
  already *defined* — "the most recent this many", defaulted to 200 and clamped at 500 — and the
  console sends it today. Decision 3 below keys the paged form on `limit` being present, which on
  that route would change the response shape of a request already in flight: exactly what this ADR
  exists to prevent. So `/admin/audit` keys its paged form on **`offset`** instead, and `limit` alone
  answers as it always has. The trigger is therefore per-route, and the rule is: where `limit` was
  free, naming it asks for a page; where `limit` already meant something, naming an `offset` does.
  Nothing else in the cohort has a pre-existing `limit`, so nothing else is affected. This is the
  fourth place this ADR's prose ran ahead of its own measurements, and the pattern is now the point:
  the numbers were collected before the decisions were written, and the decisions did not go back to
  check them.
- **`DataTable` paginates client-side today.** Its `perPage` prop slices `sorted()`, and `total` is
  `sorted().length`. F2's plan called these "reserved server-paging props"; they do not exist.
- **Three of the five lists that need paging have an index that finds their rows but cannot serve
  their `ORDER BY`.** `vouchers` is indexed `(tenant_id, campaign_id)` and ordered
  `created_at DESC, voucher_id DESC`; `catalog_items` and `employees` are both indexed `(tenant_id)`
  and ordered `created_at DESC`; `media_assets` `(tenant_id, created_at DESC)` covers the order it has
  today but not the total order decision 9 requires of it. Only `audit_log` `(tenant_id, at DESC)`
  comes close, and even there the `id` tiebreaker falls outside its index. Without the sort in the index, `LIMIT`/`OFFSET` shrinks the *response* while the database
  still sorts every matching row on every page — for a 30 000-code campaign, on every keystroke of a
  pager.

**The finding that decides this ADR.** If `?limit=` gets a default — any default — then five item
pickers, four store pickers and the menu compiler silently start seeing a truncated set. Nothing
errors. A menu compiles without the items on page two. That is the defect class this project keeps
paying for, and it is entirely avoidable: the whole-set read and the paged read are **two different
questions**, and a caller asking "give me every item so I can offer them in a dropdown" is not an
un-migrated legacy client to be phased out. It is a correct caller whose need paging cannot serve.

**A correction to `docs/cloud-admin-ux-plan.md`.** That plan says "pagination/limit/cursor on
**every** list" (§F2 audit) and "pagination on every new list" (§exit criteria), and its F2 row reads
"pagination/filter/sort/q params + read-one on every entity". On the measurement, "every" is wrong in
both directions: it would paginate four compile-time registries and thirty-four
small lists that will never need it, and — the part that matters — it would put a page
between the menu compiler and the items it compiles. The criterion in decision 3 replaces "every".
The audit line that produced it was still right about the state of the world: there was, and outside
those five windowed reads still is, no paging anywhere.

**A correction to this work's own earlier record on B1.** An earlier scoping pass for F2 Part B item
**B1** (`(tenant_id, created_at)` composite indexes on the admin tables) recorded it as a *non-task*,
on the grounds that 37 of 51 tables already carry a tenant-scoped index. That measured the wrong
property: tenant **scoping** is not sort **coverage**, and a paged read needs both. On the six lists
this ADR actually pages, three lack the second — so B1 is not a non-task. It is narrower than the plan
had it (six lists, not every table) and it is a **prerequisite of paging, not a separate item**: each
paging slice carries the additive index its own `ORDER BY` needs, in the same PR as the SQL that
depends on it, because an index landing a slice later means shipping a page that reads like paging and
costs like a full sort.

**Decision.**

**1. Paging is opt-in per request, and the unpaged read stays first-class and permanent.**

`?limit=` absent → today's response, a bare JSON array of every row, unchanged. `?limit=` present →
a paged envelope. The unpaged form is not deprecated, not legacy, and not scheduled for removal; it
is the read a picker and a compiler make. Both forms are documented as supported on every list route
that gains paging.

```
GET /admin/items?tenant_id=…              → [ {…, "etag": "…"}, … ]        (every row)
GET /admin/items?tenant_id=…&limit=25     → { "items": [ … ], "total": 812, "limit": 25, "offset": 0 }
```

`total` is the count matching the filter, not the page length, so a pager can render "1–25 of 812".
Rows keep the per-row `etag` ADR-0095 put on them; the envelope is additive around them.

**2. The seam gains a second method; the existing one is not touched.**

`list_items(tenant)` keeps its signature and every caller it has. Where paging is needed,
the trait gains `list_items_page(tenant, &PageRequest) -> Result<Page<CatalogItem>, …>`. Changing the
existing method instead — to take an `Option<PageRequest>` and return a count nobody asked for —
would churn every whole-set call site to pass `None` and unwrap a `total` it discards, for no gain.
`AuditStore` already has this shape: `query(filter)` sits beside `list_recent(limit)`.

```rust
/// What a caller wants of a page: how many rows, from where, matching what, ordered how.
pub struct PageRequest {
    pub limit: u32,          // 1..=MAX_PAGE_LIMIT, never defaulted
    pub offset: u32,
    pub search: Option<String>,
    pub sort: Option<SortField>,   // one of the route's declared fields
    pub descending: bool,
}

/// One page and the size of the set it came from.
pub struct Page<T> { pub items: Vec<T>, pub total: u32 }
```

**3. A list gets a paged method only when both halves are true.**

(a) a console screen renders the whole set into the DOM, and (b) the row count can plausibly reach a
size one response should not carry. Both, because (a) alone describes every list and (b) alone
describes tables nobody looks at.

**Amended during B3-2.** (b) first read "the row count is driven by data volume or fleet size rather
than by a human authoring each row". That is a cleaner-sounding line and it is wrong twice over. It
reads as a binary when what matters is magnitude: a chain's `items` and a company's `employees` are
each authored by a human *and* number in the thousands, so the original wording excluded two lists it
was written to include. And applied to `devices` it produced a straightforwardly wrong answer —
see below.

| Cohort | Lists | Why |
|---|---|---|
| **Paged** (5) | `vouchers`, `media`, `audit`, `items`, `employees` | 10 000-per-batch mints; one row per uploaded asset; one row per console write; a chain's item master in the thousands; headcount in the thousands |
| **Not paged** (34) | `admins`, `alerts`, `api_keys`, `areas`, `assignments`, `brands`, `campaigns`, `devices`, `display_categories`, `display_subcategories`, `fleet`, `ingredients`, `invites`, `item_categories`, `item_subcategories`, `layout_buttons`, `menu_sections`, `menus`, `modifier_groups`, `placements`, `recipes`, `roles`, `routing`, `scheduled`, `sessions`, `stations`, `stores`, `suppliers`, `table_qr`, `tables`, `tax_classes`, `tax_rates`, `tenants`, `webhooks` | too few rows to page, or already windowed |
| **Never** (4) | `permissions`, `capabilities`, `countries`, `locales` | compile-time registries |

**`devices` was in the paged cohort and should not have been.** This record justified it with "fleet
size", which is not what the route reads: `GET /admin/stores/{store_id}/devices` lists **one store's**
devices — a few terminals, a KDS, a printer or two — so the count is bounded by what someone installs
in one shop and fleet size never enters it. Caught when B3-2 went to build it and read the route's
own path. It is authoring-bounded like `areas` and `tables`, and moves to the row above; the paged
cohort is five, not six.

`recipes` and `suppliers` are the remaining judgment call: recipe count tracks the item master, so it
will follow `items` if that ever bites. They stay unpaged here because `Inventory.tsx` reads all three
lists together to render one editor, and paging one of the three buys nothing while the other two are
read whole. Revisit when the item master, not the plan, says so.

**4. `?q=` is allowed without `?limit=`, because that is what a picker needs.**

A dropdown over 3 000 items does not want page four; it wants type-ahead. So `q` filters the unpaged
array too, and returns an array. It is a case-insensitive substring match on the route's declared
searchable text fields, bound as a parameter — never interpolated. `q` present with `limit` filters
first and counts the filtered set, so `total` is what the pager should show.

**5. `sort` is a whitelist per route, not a column name.**

Each paged route declares its sortable fields; anything else is `400 INVALID_ARGUMENT` naming the
field and listing the accepted set, the convention Q3b-4a already established. Two reasons, and the
second is the one that matters: an arbitrary identifier reaching `ORDER BY` is an injection surface
no parameter binding covers, and an unindexed sort on a large table is the performance problem paging
was supposed to solve. `order` is `asc` or `desc` — also a closed set, also refused by name.

**6. `limit` and `offset` are bounded, and a bad one is refused rather than clamped.**

`limit` must parse as an integer in `1..=MAX_PAGE_LIMIT` (**500**); `offset` must be a non-negative
integer at most `MAX_PAGE_OFFSET` (**100 000**). Out of range is `400`, naming the field and the
accepted range. Clamping silently would let a client believe it read the tail of a set it never
reached — the same silence this ADR exists to refuse. A deep offset past the cap is the signal to
reach for cursor paging, which this ADR does not decide.

**7. `total` is exact, from the same statement as the rows.**

`COUNT(*) OVER()` on the windowed `SELECT`: one round trip, one snapshot, so the count cannot
disagree with the page it labels. An estimate from `pg_class.reltuples` is cheaper and makes the
pager lie about a set the admin is actively editing.

**8. `DataTable` gains optional server-paging props and stops guessing.**

When `total`, `onPage` and (where offered) `onSort`/`onQuery` are passed, the component renders the
pager from `total` and delegates: it does not sort locally, does not slice locally, and does not
compute `pageCount` from the rows it happens to hold. When they are absent it behaves exactly as it
does today. A table that is handed 25 of 812 rows and left to paginate them client-side shows "1–25
of 25", which is worse than no pager.

**9. A paged read requires a total order, and the index must cover all of it.** *(Added by amendment
during B3-2.)*

`LIMIT`/`OFFSET` selects a window of a sorted sequence. If the `ORDER BY` does not distinguish every
row, there is no sequence to take a window of: the database may return tied rows in any order, and two
queries an admin makes seconds apart — page one and page two — can order them differently. The result
is a row on both pages, or on neither. That is a **correctness** failure, not a performance one, and
it hides in testing because a given plan usually happens to be stable.

Every `created_at` in this schema is `timestamptz NOT NULL DEFAULT now()`, and PostgreSQL's `now()`
is **transaction** time — so every row written by one transaction carries the *identical* timestamp,
not merely a close one. Measured on the real database: six `media_assets` rows inserted in one
transaction produced **one** distinct `created_at`. Any list ordered `created_at DESC` alone is
therefore unordered across each batch it contains, and there is a live batch path — the CSV import
rail loads a whole item file in one transaction.

So a list joining the paged cohort must **order by something total** — the timestamp plus the row's
own key, e.g. `ORDER BY created_at DESC, media_id DESC` — and **have an index covering that whole
order**, because by decision 8's reasoning a tiebreaker the index does not carry puts the `Sort` node
straight back.

`vouchers` already satisfied both (`created_at DESC, voucher_id DESC`), which is why B3-1 was sound —
by inheritance from the query that was already there, not by design.

**Consequences.**

- Five lists gain a second seam method, a second SQL statement, a fake implementation and a paged
  route form; three of them also gain an additive index so the page is cheap and not merely small.
  Thirty-four keep exactly what they have.
- **Migrations:** four additive `CREATE INDEX IF NOT EXISTS` — one per paged list whose index does
  not cover its total order — forward-only and idempotent per ADR-0017. This ADR carries none itself.
- Two response shapes exist per paged route. That is the cost of decision 1 and it is deliberate: the
  alternative was one shape and a truncated menu compile.
- `docs/openapi.json` must describe both forms for the paged routes. It currently describes two paths
  in total, so this lands with whatever B5 decides and does not wait on it.
- The unpaged read on `vouchers` remains able to return 30 000 rows. Paging is offered, not imposed,
  so the console must actually pass `limit` for the acute case to improve — that wiring is part of the
  same slice as the route, never a follow-up.
- No `pos-ports` port changes, no `pos-proto` change, no `PROTOCOL_VERSION` bump: every `/admin`
  addition here is additive and opt-in.

**Not decided here.**

- **Cursor/keyset paging.** Needed only once `MAX_PAGE_OFFSET` is a real limit rather than a
  backstop. `total` and `offset` would both change meaning, so it wants its own ADR.
- **`If-None-Match` on lists.** Still a bandwidth question, still out of scope — ADR-0094's exclusion
  stands for reads.
- **Server-side paging for `/v1`.** The tenant-facing API's windowed reads are ADR-0081's; nothing
  here changes them.

**Delivery.** Three slices, in this order:

1. **B3-1** — the vocabulary (`PageRequest`, `Page<T>`, the parse-and-refuse helper, the caps, the
   sort whitelist type), plus `vouchers` end-to-end: an additive migration extending
   `vouchers_by_campaign` to carry `created_at DESC, voucher_id DESC`, the seam method, store-postgres
   SQL with `COUNT(*) OVER()`, fake, route, typed client, `DataTable` server props, and
   `Campaigns.tsx` actually passing a limit. One acute case proven all the way through before the
   mechanical cohort.
2. **B3-2** — `media`, `audit` and `items` on the vocabulary B3-1 fixed, with `employees` held back
   (see below). Each gains the total order decision 9 requires and an index covering it: a tiebreaker
   plus a new index for `media_assets` (migration 0041) and `catalog_items` (0043); for `audit_log`,
   whose order is already total, only the widened index (0042).

   **Delivered.** Three notes on what the slice actually found:

   - **`audit`'s trigger is `offset`, not `limit`** — see the correction under the fifth measurement
     bullet above.
   - **`employees` is held, not dropped.** It qualifies on headcount, but it is T1 personal data and
     that call is the owner's, not this ADR's. Nothing about the paged read would change which fields
     leave the server or what reaches a log — the gate is the same `console.people.manage`, and a
     page carries *less* data than the whole-set read it sits beside — but the criterion in this ADR
     produced one confidently wrong answer already (`devices`), so the question was asked rather than
     assumed. Until it is answered, `GET /admin/employees` is unchanged.
   - **`items` gains the API but not the screen.** The Items sub-screen finds a row with a
     client-side search box over the whole item master; server-paging it before `?q=` exists would
     leave an operator managing thousands of items with only prev/next. So the seam, adapter, index
     and route land here and the screen moves in B3-3, in the same change as the search that has to
     move with it. The Audit screen *did* move, because its three server-side filter fields already
     search the whole trail — its client-side box only ever saw the newest 200 rows, so removing it
     is a fix rather than a loss.
3. **B3-3** — `q`/`sort`/`order` across the paged cohort, each route declaring its own fields.

   **Scoped down on measurement, and `items` delivered.** "Across the cohort" turned out to name
   less work than it sounds, because the cohort's screens do not all want these:

   - **`items` has both, and needed them** — its sub-screen finds a row by typing, which is why B3-2
     could not move that screen. `?q=` matches the name or any per-locale name; `?sort=` offers
     `newest`, `name` and `status` from a closed enum, with migration 0044 covering the name order.
     The screen now pages, searches and sorts server-side.
   - **`media` wants neither.** It is a grid of thumbnails with no search box and no sortable
     header; adding a `?q=` nothing calls would be the shape of finding #273.
   - **`audit` wants `sort`, not `q`.** Its three exact-match filters already search the whole trail,
     and a free-text `?q=` over a log that reaches millions of rows is a different problem: no
     substring predicate can use a btree, so it would be a full scan per page. That is a trigram-index
     decision (a new dependency, so its own ADR) and it is deliberately not made here. `sort` on
     `audit_log`'s own columns is cheap and is the remaining piece of this slice.

     **Correction, on building it: `audit` wanted `order`, not `sort`.** This is the fifth time this
     ADR's prose ran ahead of what the screen actually needs, and the reasoning that produced the
     line above is the same each time — it counted what was *cheap to index* instead of what an
     operator asks. The Audit screen has four sortable headers: the instant, the acting admin, the
     action, and the entity type. The last three each already have an **exact filter** directly above
     the table. "Show me this admin's changes" is the filter's question, and the filter answers it
     over the whole trail; ordering a million-row trail by a low-cardinality column answers nothing
     the filter does not answer better, and would need an index per column to do it. So no `?sort=`
     shipped, and those three headers stopped being controls — until this slice they sorted the
     twenty-five rows on screen as if they were the trail.

     What the instant column needed was not a sort field but a *direction*: `?order=newest|oldest`.
     An incident reads oldest-first — a `since_ms`/`until_ms` window in the order the actions
     happened — and that is the one ordering question this screen has.

     Three consequences worth recording:

     - **No migration.** `audit_log_by_tenant_newest` (0042) is `(tenant_id, at DESC, id DESC)` and
       the oldest-first order is that index read backwards. Verified against real PostgreSQL: the
       plan for `ORDER BY at ASC, id ASC` names that index and carries no sort step. Decision 9's
       requirement (a total order, ending in the primary key) is what makes the reversal exact — an
       `at ASC, id DESC` would still be total, would still look right on one page, and would no
       longer be that index; the plan guard catches it with `Sort Key: at, id DESC`.
     - **The tokens are `newest`/`oldest`, not `asc`/`desc`.** On `/admin/catalog/items` the
       direction is relative to a named `?sort=` field, so `asc` means "the sort's natural
       direction" — for `sort=newest` that is `created_at DESC`. This route has no `sort`, so `asc`
       here would have to mean "ascending in time": one parameter name meaning two different things
       across two routes. Two spellings with a closed-set refusal on each is the smaller cost.
     - **The order lives on the paged read only.** On the windowed read `?limit=` already means "the
       most recent this many", so `?order=oldest&limit=200` reads either as the newest two hundred
       shown earliest-first or as the earliest two hundred — different sets, both honest. The route
       refuses the parameter there rather than choosing. `LIMIT`/`OFFSET` over an ordered set has no
       such ambiguity, which is why the paged form can carry it: order first, window second, always.

     This is the same shape as the `offset`-not-`limit` correction recorded above, and it has the
     same cause: a `limit` that already meant something on this route before paging existed.
   - **`vouchers` wants neither** — no console screen renders a table of codes at all; the read
     serves a count and an operator printing a flyer run.

   **A guard's limit, found by mutation.** The `EXPLAIN` tests each assert the plan of a query the
   *test* writes, so they catch a migration being dropped but not the adapter's own `ORDER BY`
   losing its tiebreaker — and the page-partition tests cannot catch that either, because with the
   index present the index walk supplies the missing order for free. Removing `menu_item_id` from
   the name fragment passed every test in the tree. The gap is closed by asserting the invariant
   where it lives: a unit test over the `ORDER BY` fragments requiring each to end in the primary
   key, which covers a variant added later the day it is written.

   **A live defect this slice fixed.** B3-2 left `DataTable` sorting the page in server mode while
   its docstring said otherwise, which the Audit screen made reachable. Server-mode sorting is now
   gated on the column naming a server field *and* the caller offering `onSort`; headers are inert
   otherwise rather than quietly ordering a window.
