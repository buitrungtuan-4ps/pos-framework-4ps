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
  **10 000** per mint and batches accumulate per campaign, so `Campaigns.tsx` fetches and renders
  every code a promotion ever issued — three drops is 30 000 rows in one response. `media` grows one
  row per uploaded asset, `audit` one per console write, `devices` with the fleet, `employees` with
  headcount, `items` with the chain's menu. Everything else is bounded by how much a human typed.
- **Four lists are closed sets with no table behind them at all** — `permissions`, `capabilities`,
  `countries`, `locales` are compile-time registries. Paging them is ceremony.
- **Five query structs already carry a `limit`** — `AuditReadQuery`, `AlertListQuery`,
  `ReconcileHistoryQuery` and the `/v1` and `/admin` rollup windows — from ADR-0081 and ADR-0069.
  None carries `offset` or `total`, and `ScheduledListQuery` and `OtaRolloutQuery` carry neither
  despite being history reads.
- **`DataTable` paginates client-side today.** Its `perPage` prop slices `sorted()`, and `total` is
  `sorted().length`. F2's plan called these "reserved server-paging props"; they do not exist.

**The finding that decides this ADR.** If `?limit=` gets a default — any default — then five item
pickers, four store pickers and the menu compiler silently start seeing a truncated set. Nothing
errors. A menu compiles without the items on page two. That is the defect class this project keeps
paying for, and it is entirely avoidable: the whole-set read and the paged read are **two different
questions**, and a caller asking "give me every item so I can offer them in a dropdown" is not an
un-migrated legacy client to be phased out. It is a correct caller whose need paging cannot serve.

**A correction to `docs/cloud-admin-ux-plan.md`.** That plan says "pagination/limit/cursor on
**every** list" (§F2 audit) and "pagination on every new list" (§exit criteria), and its F2 row reads
"pagination/filter/sort/q params + read-one on every entity". On the measurement, "every" is wrong in
both directions: it would paginate four compile-time registries and thirty-three
authoring-bounded lists that will never need it, and — the part that matters — it would put a page
between the menu compiler and the items it compiles. The criterion in decision 3 replaces "every".
The audit line that produced it was still right about the state of the world: there was, and outside
those five windowed reads still is, no paging anywhere.

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

(a) a console screen renders the whole set into the DOM, and (b) the row count is driven by data
volume or fleet size rather than by a human authoring each row. Both, because (a) alone describes
every list and (b) alone describes tables nobody looks at.

| Cohort | Lists | Why |
|---|---|---|
| **Paged** (6) | `vouchers`, `media`, `audit`, `items`, `employees`, `devices` | 10 000-per-batch mints; one row per asset; one per console write; a chain's item master; headcount; fleet size |
| **Not paged** (33) | `admins`, `alerts`, `api_keys`, `areas`, `assignments`, `brands`, `campaigns`, `display_categories`, `display_subcategories`, `fleet`, `ingredients`, `invites`, `item_categories`, `item_subcategories`, `layout_buttons`, `menu_sections`, `menus`, `modifier_groups`, `placements`, `recipes`, `roles`, `routing`, `scheduled`, `sessions`, `stations`, `stores`, `suppliers`, `table_qr`, `tables`, `tax_classes`, `tax_rates`, `tenants`, `webhooks` | bounded by authoring effort, by fleet size at a scale a page does not help, or already windowed |
| **Never** (4) | `permissions`, `capabilities`, `countries`, `locales` | compile-time registries |

`recipes` and `suppliers` are the judgment call: recipe count tracks the item master, so it will
follow `items` if that ever bites. It stays unpaged here because `Inventory.tsx` reads all three
lists together to render one editor, and paging one of the three buys nothing while the other two
are read whole. Revisit when the item master, not the plan, says so.

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

**Consequences.**

- Six lists gain a second seam method, a second SQL statement, a fake implementation and a paged
  route form. Thirty-three keep exactly what they have.
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
   sort whitelist type), plus `vouchers` end-to-end: seam method, store-postgres SQL with
   `COUNT(*) OVER()`, fake, route, typed client, `DataTable` server props, and `Campaigns.tsx`
   actually passing a limit. One acute case proven all the way through before the mechanical cohort.
2. **B3-2** — `media`, `audit`, `items`, `employees`, `devices` on the vocabulary B3-1 fixed.
3. **B3-3** — `q`/`sort`/`order` across the paged cohort, each route declaring its own fields.
