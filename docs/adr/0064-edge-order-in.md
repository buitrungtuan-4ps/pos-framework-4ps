# ADR-0064 — The edge implements `OrderIn` over its own application layer, repricing from the synced menu catalog

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-24
**Relates to** [ADR-0026](0026-port-shapes.md) · [ADR-0063](0063-store-menu-catalog.md) · [ADR-0056](0056-public-order-intake.md) · [ADR-0061](0061-order-relay.md) · [ADR-0057](0057-qr-ordering.md) · [ADR-0001](0001-offline-first-store-autonomy.md)

**Context.** [ADR-0026](0026-port-shapes.md) §5 makes the store’s edge the real `OrderIn` implementor — it
reprices from its own menu, routes to the kitchen, and **must accept offline** (contract rule 5,
[ADR-0001](0001-offline-first-store-autonomy.md)). Everything upstream now exists: the cloud serves
`POST /v1/orders` ([ADR-0056](0056-public-order-intake.md)), relays it to the store as a queue the
store pulls ([ADR-0061](0061-order-relay.md)), and QR ordering ([ADR-0057](0057-qr-ordering.md))
rides the same port. And [ADR-0063](0063-store-menu-catalog.md) gave the store the price book it
needs. What was missing is the store-side `OrderIn` itself — the thing the relay client calls.

The edge’s application layer only knew **table-service** orders: `seat_table` opens an order *on a
table*. An inbound order may have no table (delivery, the public API) or a table (QR), and it arrives
as `(menu_item_id, quantity)` — no price, no display name, no staff member.

**Decision.**

- **`EdgeOrderIn` is a thin driving-port implementor over `Edge`.** It holds the `Edge` application,
  an idempotency **intake ledger**, and the box’s own device id (there is no signed-in employee for
  an inbound order). It is generic over the ledger type — static dispatch, no `dyn`, no boxed futures
  ([ADR-0013](0013-async-strategy.md)).

- **`submit` reprices, then opens the order.** For each line it calls
  `pos_core::menu::reprice_line` against the session’s [`MenuCatalog`] (ADR-0063) and channel-keyed
  tax table: an unknown item → `invalid_argument` (rule 3, never a substitution); an 86’d item →
  `failed_precondition`; a class with no rate on the channel → `failed_precondition`. The caller’s
  quoted price is compared and surfaced in `repriced`, **never charged** (rule 2). It then opens the
  order through a new application method `Edge::open_inbound_order`, which emits `sales.order.opened`
  (the catalogue’s tableless order-open event) and one `sales.order_line.added` per priced line in
  **one store transaction**, and folds them into the projection — so a QR order shows the table
  occupied and a takeaway order joins the kitchen queue.

- **The acceptance total is the menu total, tax-inclusive.** `OrderAcceptance::total` is the sum of
  the captured line totals — the tax-inclusive price the guest pays (Vietnam prices include VAT;
  `Money::tax_included` extracts it at bill time, it is never *added* on top). This is what the
  `OrderIn` contract suite asserts: the total comes from the store’s own menu.

- **Idempotency is `(sales_channel, external_reference)`, held in a durable per-store intake
  ledger.** The caller’s reference is not in the event log (the log carries the order’s own identity,
  not a marketplace’s), so — exactly as the cloud relay dedupes on a stored queue key
  ([ADR-0061](0061-order-relay.md)) — the edge records `(channel, reference) → (order_id,
  acceptance)` and returns the existing acceptance with `created: false` on a repeat. The production
  ledger is a `store-sqlite` table written **in the order’s own transaction** (so a crash cannot
  leave an order the ledger does not know about); the in-memory ledger is the tests-and-example
  implementation, the same split as [`ReceiptAuthority`](../../crates/pos-edge/src/receipt.rs).
  `look_up` reads the ledger — the resolution path for a caller whose `submit` timed out.

- **Staff confirmation follows the channel.** `awaiting_staff_confirmation` is true when the order
  names a table — a QR guest’s order waits for a member of staff before the kitchen sees it
  ([ADR-0057](0057-qr-ordering.md)) — and false for a delivery or public-API order, which a
  marketplace or the caller has already committed to.

- **A tableless order gets a durable, daily-resetting queue number.** An order with no table (a
  marketplace, public-API, or takeaway order) is called back by `OrderAcceptance::queue_number`,
  allocated through a `QueueNumberAuthority` injected into `EdgeOrderIn` — the same static-dispatch,
  in-memory-vs-`SqliteStore` split as the ledger and [`ReceiptAuthority`](../../crates/pos-edge/src/receipt.rs).
  Unlike the receipt number, the counter is keyed by `(store, business_date)`, so it **resets each
  trading day** with no midnight job (a date the counter has never seen starts at 1), and it is
  **durable**: it must not be an in-memory counter, because a box that lost power mid-service would
  reissue `#1` and shout the same number at two customers. Allocation is idempotent by `order_id`, so
  a retry that reached the authority yields one number, not two. A QR order names a table and is
  served there, so it gets none.

**Consequences.**

- The store side of the relay ([ADR-0061](0061-order-relay.md)) can now be closed: the pull-and-ack
  client maps each queued order to an `InboundOrder`, calls this `OrderIn`, and reports the outcome —
  the follow-up PR.
- Accepting needs no cloud: the catalog is local synced config and the order is written to the local
  event log, so a marketplace or QR order is accepted with the internet down (rule 5).
- **The durable queue number landed with this design** (`QueueNumberAuthority` + a `store-sqlite`
  `queue_counter`/`queue_allocations` pair, migration 0003), proven by a store-sqlite test that the
  sequence survives reopening the database and still restarts on a new business date.
- **Still deferred to a follow-up commit, not designed away:** the intake **ledger’s** durable
  SQLite backing. This commit ships the in-memory ledger behind the trait and the shared `OrderIn`
  contract suite that proves the dedupe behaviour; making that ledger a `store-sqlite` table written
  **in the order’s own transaction** is the tx-atomic change described above, named here so the seam
  is right.

**Rejected.**

- **Add `external_reference` to `sales.order.opened`.** It would make idempotency a property of the
  log, but it bumps a frozen event’s schema and forces a snapshot regeneration and a protocol note
  for a key that is the *caller’s*, not the order’s. The store-tx-atomic ledger gets the same
  crash-safety without touching the wire protocol, and mirrors how the cloud relay already dedupes.
- **Reuse `seat_table` / the table-service path.** A delivery or public-API order has no table; forcing
  one would invent floor state that isn’t real and couple intake to the dine-in flow.
