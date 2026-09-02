# ADR-0093 — A bill belongs to an order, not to a table

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-02
**Amends** [ADR-0064](0064-edge-order-in.md) (the tableless order this makes payable) · **Relates to** [ADR-0063](0063-store-menu-catalog.md) (the catalog a relayed order reprices from) · [ADR-0072](0072-floor-and-kitchen.md) (the table lifecycle that stops being a precondition) · [ADR-0080](0080-channels-and-payments.md) (the channels a store accepts) · [ADR-0057](0057-qr-ordering.md) (the QR order that *does* name a table) · `docs/roadmap-v3.md` (finding #9, found by writing Q1)

**Context.** **Takeaway revenue is uncollectable at the counter.** Not degraded — impossible.

`EdgeOrderIn` accepts a relayed order, reprices it from the store's own menu
([ADR-0063](0063-store-menu-catalog.md)), stores it in one transaction with its intake-ledger row,
and issues a durable daily queue number ([ADR-0064](0064-edge-order-in.md)). Then the path ends.
`Edge::open_bill` is the only route to a bill anywhere in the tree, and it is table-shaped in four
places:

1. its signature is `open_bill(actor, table_id: TableId)`;
2. it gates on `decide_table(current, TableCommand::RequestBill, ctx)`, which requires the table to
   be `Occupied`;
3. it resolves the order *through* the table, with `order_for_table(table_id)`;
4. the projection's `BillRecord` stores `table_id: TableId` — not an `Option` — and settling reads it
   to cycle that table to `NeedsCleaning`.

A takeaway order is tableless **by design**: PR-1b made `table_id` optional precisely so a
marketplace order, the public API, or a walk-in takeaway could be accepted without inventing a
table. So there is no table to pass, no table to be `Occupied`, and no table to resolve the order
from. No HTTP route opens a bill without a table, and the edge UI has no takeaway screen at all.
Every takeaway order the store accepts is priced, queued, fired — and then cannot be charged for.

This was found by writing the Q1 acceptance suite, not by a unit test, and that is the point: each
of the four couplings is individually correct and locally tested. The defect is only visible when you
try to walk a takeaway order from intake to payment, which is exactly what an in-process end-to-end
suite does and what a per-module test cannot.

**What is already right, and it changes the size of this.** The **event log has never been
table-keyed**. `BillingBillOpened` carries `{ bill_id, order_id }` and nothing else; `BillSplit`,
`BillMerged`, `BillSettled`, `BillVoided`, `DiscountApplied`, `CompApplied`, `PaymentCaptured` and
`TipAdjusted` are all keyed on `bill_id` alone. Not one billing event mentions a table. The
projection even admits it: `table_for_order` exists with the comment *"a bill event names its order,
not its table"*, reverse-engineering the table on rebuild because the log does not carry it.

So the durable truth already says a bill belongs to an order. What is table-shaped is the *edge's
in-memory reach* for it. This ADR is not introducing a new model; it is deleting a coupling the wire
never had.

**Decision.**

- **A bill is opened on an order.** The app layer grows `open_bill_for_order(actor, order_id)` as the
  primitive, and `open_bill(actor, table_id)` becomes a thin caller that resolves `table_id →
  order_id` and delegates. The table path keeps its `decide_table` gate exactly as it is — a
  dine-in bill still requires an `Occupied` table, because on the floor that gate is what stops a
  bill being requested for a table nobody is sitting at.

  Keeping both entry points is deliberate. Collapsing them into one order-keyed call and making the
  UI resolve the table itself would move the floor gate out of the domain and into a caller, and the
  `RequestBill` transition is a floor move that ADR-0072 owns. The order-keyed primitive is the
  *general* case; the table-keyed one is the floor's convenience over it.

- **`BillRecord.table_id` becomes `Option<TableId>`,** and settling cycles a table only when there is
  one. A takeaway bill settles, prints, and takes payment without touching the floor. This is the
  change that makes "no table" representable rather than an error, and it is confined to the
  projection — an in-memory structure rebuilt from the log, so **no migration**.

- **A takeaway bill is refused if the order is not payable**, and payable means: the order exists,
  belongs to this store, and has no bill already open on it. That last one replaces what the table
  gate was implicitly providing. A table can only be `Occupied` once, so `order_for_table` could
  never hand out the same order twice; keyed on an order directly, nothing stops two `open_bill`
  calls on one order unless we say so. So the projection gains an order → bill index and the
  primitive refuses a second bill on an order that already has an open one.

  This is the part worth being careful about: it is the invariant the table state machine was
  enforcing as a side effect, and moving to an order key would silently drop it. Two open bills on
  one order means two receipts and double payment for one meal.

- **No `pos-proto` change and no `PROTOCOL_VERSION` bump.** The events already carry what is needed.
  A takeaway bill emits the same `BillingBillOpened { bill_id, order_id }` a dine-in bill does, and
  the cloud, the rollups and the ERP posting cannot tell them apart — correctly, because a sale is a
  sale.

- **The HTTP surface and the UI are the second half, in the same slice.** A domain change nothing can
  reach is the exact pattern this roadmap keeps catching (finding #1, and the seven slices Q1 found
  unreachable). So the slice includes the route that opens a bill on an order and the edge-UI screen
  that lists queued takeaway orders and takes payment for one. Without those, takeaway revenue is
  still uncollectable and the ADR would be describing a capability the counter does not have.

**Consequences.**

- **`open_bill`'s callers are unchanged**, so no dine-in behaviour moves. The floor gate, the
  `Occupied` precondition, and the settle-cycles-the-table behaviour are all preserved for the path
  that has a table.
- **`BillView` gains an optional table.** Anything rendering a bill has to cope with its absence,
  which the takeaway screen wants anyway (it shows a queue number where a dine-in bill shows a table
  label).
- **Rebuild gets simpler, not harder.** `table_for_order` returning `None` stops being a case to
  work around and becomes the honest answer for a takeaway bill.
- **Two open bills on one order become expressible in the type system and refused in the domain.**
  The refusal needs a test, and it needs to be a test that fails if the index is removed — an
  invariant enforced only by a comment is the one this ADR exists to stop trusting.
- **The queue number becomes the takeaway bill's human handle.** It is already durable and daily
  (PR-1c); this gives it the job it was minted for.

**Alternatives considered.**

- **Give every takeaway order a synthetic table.** No domain change at all: mint a hidden `TableId`
  per takeaway order, mark it `Occupied`, and the existing path works untouched. Rejected, and it is
  the tempting one. It puts rows that are not tables into the floor plan, so the floor screen, the
  table count, the `NeedsCleaning` queue and every future floor report inherit phantom entries that
  each need excluding — and the exclusion is invisible until someone counts tables and gets the
  wrong number. It also makes `TableId` mean two things, which is how an id stops being trustworthy.
  A synthetic key to satisfy a precondition is a lie the schema then has to keep.
- **A separate `TakeawayBill` type and a parallel settle path.** Rejected: it doubles the payment,
  discount, comp, tip and void paths — the places where money is computed and where a divergence
  would be a real financial bug — to avoid one `Option`. Two settle paths mean two chances to get
  rounding, tax or the cash-drawer rollup wrong, and only one of them gets exercised by the dine-in
  tests everyone runs.
- **Let the cloud open the bill and push it down.** Rejected on [ADR-0001](0001-offline-first-store-autonomy.md):
  a store must be able to take money with the link down, and a takeaway counter is exactly where a
  queue of waiting customers makes that non-negotiable.
- **Widen `open_bill` to take `Option<TableId>` instead of adding a primitive.** Rejected on
  call-site honesty: every existing caller would keep compiling while silently gaining a new legal
  argument, and the floor gate would have to become conditional inside one function that now means
  two things. A separate primitive makes the general case explicit and leaves the floor path
  unambiguous.

**Deliberately not in this slice.**

- **Splitting or merging a takeaway bill.** Both are keyed on `bill_id` already, so neither is
  blocked by this — but neither has a counter workflow, and shipping a domain capability with no
  screen is the pattern above. When a counter needs it, it is additive.
- **The three residual hardcoded `VND` sites** (E5) are untouched. The takeaway screen must not add a
  fourth: every figure it shows comes from the edge, per E5's own rule.
