# ADR-0064 — The edge implements `OrderIn` over its own application layer, repricing from the synced menu catalog

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-02
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

- **Idempotency is `(sales_channel, external_reference)`, held in a durable intake ledger written in
  the order’s own transaction.** The caller’s reference is not in the event log (the log carries the
  order’s own identity, not a marketplace’s), so — exactly as the cloud relay dedupes on a stored
  queue key ([ADR-0061](0061-order-relay.md)) — the edge records `(channel, reference) → record` and
  returns the existing acceptance with `created: false` on a repeat. This is a **`pos-ports` port**,
  `IntakeLedger`, and — like `ConfigStore` — a [`Transactional`](../../crates/pos-ports/src/tx.rs)
  one: `record` buffers into the caller’s transaction, so the ledger row and `sales.order.opened`
  **commit together or not at all** and a crash between them is impossible. `store-sqlite` implements
  it against a table (migration 0004); the fake implements it in memory; both are proven by the
  shared `OrderIn` contract suite. The key is a **plain** insert, not insert-or-ignore: a second
  order racing in on the same key fails its commit with `already_exists` (the one writer thread
  serialises the two) and rolls back rather than duplicating, and `submit` resolves the loss by
  looking the key up. `look_up` also serves a caller whose `submit` timed out. The `queue_number` is
  **not** stored in the record — it is reconstructed from the (order-keyed, idempotent) queue
  authority on a repeat, so a crash that opened the order but never numbered it still yields exactly
  one number; the record stores the `business_date` so that reconstruction keys the right day.

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
- **The durable, tx-atomic intake ledger landed too** (`IntakeLedger` as a `pos-ports` port +
  a `store-sqlite` `intake_ledger` table, migration 0004, plus the fake’s in-memory implementation),
  proven by a store-sqlite test that a recorded key survives reopening the database and that a
  duplicate key is refused with `already_exists` rather than opening a second order. Nothing in the
  order-intake path is left in-memory now: both the dedupe ledger and the queue counter are durable.

**Rejected.**

- **Add `external_reference` to `sales.order.opened`.** It would make idempotency a property of the
  log, but it bumps a frozen event’s schema and forces a snapshot regeneration and a protocol note
  for a key that is the *caller’s*, not the order’s. The store-tx-atomic ledger gets the same
  crash-safety without touching the wire protocol, and mirrors how the cloud relay already dedupes.
- **Reuse `seat_table` / the table-service path.** A delivery or public-API order has no table; forcing
  one would invent floor state that isn’t real and couple intake to the dine-in flow.

---

## Amendment — `IntakeLedger` is registered as a port (2026-09-02)

This record called `IntakeLedger` a port when it landed, and it was built like one — a trait in
`pos-ports`, a `store-sqlite` implementation, an in-memory fake. But it was never given a
`PortName` variant, and everything downstream of that registry quietly skipped it:

- `pos-contract-tests`' `SUITES` had no entry, so there was **no shared contract suite** — the two
  implementations were never checked against each other, only against their own crates' tests.
- `docs/architecture.md` §5 had **no row**, so the authoritative port list was wrong by one.
- Its failures were reported under `PortName::OrderIn`, merging two seams into one metric label.
- The `every_port_has_a_suite` guard **could not see it at all**, because that guard iterates
  `PortName::ALL`. A trait added to `pos-ports` without a variant is invisible to it.

**What changed.** `PortName::IntakeLedger` exists (the nineteenth), with the `ALL` entry and the
`intake_ledger` label. There is a six-case suite in `pos_contract_tests::intake_ledger`, and both
`pos-fakes` and `store-sqlite` run it and pass — so "the ledger's two implementations agree" is now
checked rather than assumed. `docs/architecture.md` §5 has the row and reads nineteen. The ledger's
own errors carry `PortName::IntakeLedger`; the queue-number allocator keeps `PortName::OrderIn`,
which is the seam it belongs to.

**Nothing about the design changed.** The port is still `Transactional` sharing `EventStore`'s `Tx`,
the key is still a plain insert whose duplicate loses at commit, and no migration was needed. This
amendment is bookkeeping catching up with a decision this record already made.

**The lesson worth keeping.** A guard that enumerates a registry can only check what was registered;
it can never be what catches an unregistered thing. The control that should have caught this is the
rule that a new port needs an ADR first (ADR-0021) plus a reviewer reading it — a process control,
not a test. That is the one that failed, and the blind spot is now written down in
`docs/architecture.md` §5, in the guard's own comment, and here.
