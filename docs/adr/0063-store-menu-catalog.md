# ADR-0063 — The store server holds an authoritative menu catalog, synced as configuration, and reprices inbound orders from it

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-24
**Relates to** [ADR-0026](0026-port-shapes.md) · [ADR-0004](0004-cloud-owned-configuration.md) · [ADR-0033](0033-config-tree.md) · [ADR-0056](0056-public-order-intake.md) · [ADR-0061](0061-order-relay.md)
**Unblocks** the edge `OrderIn` (`docs/roadmap.md` P11 "OrderIn first"), and with it the store side of the order relay ([ADR-0061](0061-order-relay.md)) and QR ordering ([ADR-0057](0057-qr-ordering.md)).

**Context.** [ADR-0026](0026-port-shapes.md) §5 fixes the `OrderIn` contract, and two of its rules are
non-negotiable: **rule 2 — the store's price wins** (an implementation reprices from its own menu and
reports a differing quote in `repriced`, never charging the caller's quoted price), and **rule 3 — an
unknown menu item is `invalid_argument`, never a substitution**. The store is where an inbound order
(a marketplace order, `POST /v1/orders`, a QR order) is actually accepted ([ADR-0061](0061-order-relay.md)):
the cloud only relays it there.

But the store server has no prices. The whole design to date pushes the menu to the *device*: an
order line is drafted from "the amounts the device captured from the menu it holds"
(`crates/pos-edge/src/http/lines.rs`), and `sales.order_line.added` records exactly those captured
amounts — "a line never references the live menu" (`docs/pos-spec.md` §14.2). That is right for a
waiter tapping a tablet: the device holds the menu, so the edge records, it does not price.

An inbound order has **no device in the loop**. A marketplace or a QR guest sends menu item
identifiers and quantities — not a display name, not a store price, not a tax class. Something at the
store must turn `(menu_item_id, quantity)` into a priced line, or `OrderIn` rules 2 and 3 cannot be
honoured: there is nothing to reprice *from*, and nothing to check an item is even sold. Trusting the
caller's quoted price instead is precisely the bug the port doc names — "a menu update loses money for
a day".

**Decision.**

- **The store carries an authoritative menu catalog.** A `MenuCatalog` (in `pos-proto`, alongside
  `TaxRateTable` and the locale pack — the serializable configuration shapes) is a list of
  `MenuEntry` rows, one per sellable item: `menu_item_id`, `display_name`, `unit_price` (integer
  [`Money`](../../crates/pos-proto/src/money.rs)), `tax_class_id`, and an `available` flag. It is a
  read model — a price book — computing nothing; it is the device's menu made authoritative at the
  server for the channels that have no device.

- **It is cloud-owned configuration, delivered by the same path as everything else**
  ([ADR-0004](0004-cloud-owned-configuration.md), [ADR-0033](0033-config-tree.md)). The catalog rides
  the config tree under the `menu` node of a store's effective document and is synced to the edge as
  part of the config snapshot — no new endpoint, no new sync channel. An operator edits prices in the
  back office and the store adopts them on the next config apply, hot, without a deploy (the same
  last-known-good mechanism the rest of the config uses).

- **Repricing is domain logic, and lives in `pos-core`** (`pos_core::menu::reprice_line`), a pure
  function over the catalog — no I/O, no `pos-ports` dependency, testable in microseconds like the
  rest of the domain. Given a `RequestedLine` `(menu_item_id, quantity, modifier_menu_item_ids,
  quoted_unit_price)`, a channel, and the store's `TaxRateTable`, it returns a `PricedLine` carrying
  exactly the fields a `sales.order_line.added` event captures — `unit_price`, `line_total`,
  `tax_class_id`, `tax_rate`, `display_name`, and a `repriced` flag — or a `RepriceError`:
  - **`UnknownItem`** — the item (or a chosen modifier) is not in the catalog. Refused, never
    substituted (rule 3). Maps to `invalid_argument` at the port.
  - **`Unavailable`** — the item is in the catalog but 86'd (`available: false`). A store that has run
    out refuses the line rather than promising a dish it cannot make. Maps to `failed_precondition`.
  - **`MissingRate`** — no tax rate is configured for the item's class on this channel. This reuses
    `TaxRateTable::rate_for`'s deliberate "a missing rate is a configuration error, not a silent
    zero" decision — charging no tax on an unclassified item is a bug found by a tax audit. Maps to
    `failed_precondition`.

- **Tax composes with what already exists.** The catalog gives an item its `tax_class_id`; the
  store's channel-keyed `TaxRateTable` (D6, already carried on the session) gives the rate for that
  class *on the order's channel*. This is why the same item can be 8% takeaway and 10% dine-in with
  one catalog: the price book is channel-agnostic, and the channel selects the rate at reprice time.

- **Modifiers are catalog items too.** A chosen modifier is a `menu_item_id` in the same catalog with
  its own `unit_price`; the line's unit price is the base plus the sum of its modifiers ("modifiers
  are optional additions priced when chosen", `pos-core` inventory §8). An unknown or 86'd modifier
  refuses the line exactly as an unknown base item does.

**Consequences.**

- The edge can finally implement `OrderIn` for real (the follow-up PR): reprice each inbound line
  through `reprice_line`, refuse an unknown or unavailable item, and accept **offline** — the catalog
  is local synced config, so acceptance needs no cloud (contract rule 5, [ADR-0001](0001-offline-first-store-autonomy.md)).
- The device-priced dine-in path is **unchanged**. This catalog is the price source for channels with
  no device; a waiter's tablet still captures its own menu and the edge still records what it
  captured. The two coexist, and both ultimately write the same priced `sales.order_line.added`.
- Populating the `menu` config node from the back office (the dashboard's menu editor) is a separate
  piece of cloud work; this ADR fixes the *shape* the store reads and the *repricing* it does. Until a
  store has a menu published, its catalog is empty and it accepts no inbound order — a safe default
  (it never guesses a price), and the visible signal that the menu has not been configured yet.

**Rejected.**

- **Trust the caller's `quoted_unit_price`.** Violates rule 2 outright; loses margin silently on every
  order after a price change until someone notices. The quote stays advisory — compared, reported as
  `repriced`, never charged.
- **A separate menu port + adapter (a `MenuStore`).** The menu is configuration, and configuration
  already has an owner, a delivery path, and a last-known-good story. A parallel port would duplicate
  all three and split "what the store is configured to be" across two mechanisms.
- **Put the catalog type in `pos-core`.** It is a serialized config shape that both the cloud
  (validation, publishing) and the edge (repricing) read, so it belongs in `pos-proto` with the other
  config shapes; only the *logic* that consumes it is domain, and that is what lives in `pos-core`.
