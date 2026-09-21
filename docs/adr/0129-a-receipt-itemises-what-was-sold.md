# ADR-0129 — A receipt itemises what was sold

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-21
**Extends** [ADR-0100](0100-receipt-and-ticket-printing.md) (the document this adds a block to) · [ADR-0106](0106-the-store-is-a-legal-person.md) (the seller's identity it prints beside)
**Relates to** [ADR-0025](0025-receipt-number-authority.md) (the number, which is not an invoice number) · [ADR-0104](0104-multi-component-and-inclusive-tax.md) (the tax block that already prints, and sums to these rows)

**Context.** `pos_edge::printing::receipt_document` composes the guest's receipt from four things: the store profile, the receipt number, a `BillTotals`, and an optional buyer. **It is never given the lines.** The document goes from the seller's address straight to `Subtotal`.

So a guest is handed a total and no way to check it, and a tax authority is handed a document that names no goods. The store knows perfectly well what it sold — `sales.order_line.added` has carried `display_name`, `quantity`, `unit_price` and `line_total` since the event was written, which is precisely the "line snapshot" `docs/pos-spec.md` §14.2 requires. The projection then **dropped the first two on the way in**: `LineRecord` keeps `menu_item_id`, `quantity`, `line_total`, `tax_class_id` and the modifier ids, because those are what a fire and a tax-class fold need, and nothing has ever asked it for the rest.

This is not a preference about receipt design. Every regime the product is built for requires the itemisation:

- **Vietnam** — Decree 123/2020/NĐ-CP Art. 10 lists the contents of an invoice, per line: name, unit of measure, quantity, unit price and amount. Circular 78/2021/TT-BTC carries it to the invoice generated from a cash register, which is what this document is.
- **Japan** — the qualified invoice (適格請求書) names the goods and the amount per tax rate, and a receipt that names no goods cannot be one.
- **India** — CGST Rule 46 requires the description, quantity and value per line.
- **EU** — the VAT Directive's invoice contents include the quantity and nature of what was supplied; member-state simplified-receipt rules relax other fields, not this one.
- **France (NF525) and Germany (KassenSichV / DSFinV-K)** treat the line detail as part of the record the till must keep and produce.

The product is live in Vietnam and ADR-0005 and ADR-0025 already shape the numbering around it. A receipt with no lines is the one part of the document that is wrong in every market at once.

**Decision.**

1. **The receipt lists one row per non-voided line**, in the same order and from the same records the totals were assembled from, carrying **name, quantity, unit price and line amount**. Those rows sum to `subtotal` by construction, because they *are* the records `class_bases` folded into the tax classes. That identity is the point of doing it this way rather than re-deriving anything, and it is asserted as a test rather than assumed.

2. **Nothing new is recorded.** The data is already in the log; the change is that `LineRecord` stops discarding `display_name` and `unit_price`. No `pos-proto` change, no event change, no `PROTOCOL_VERSION` bump, no migration — a store that upgrades prints fuller receipts from the log it already has, including for bills it replays at start-up.

3. **What prints is the snapshot, not the live menu.** An item renamed or repriced after the sale does not change a settled bill (§14.2). This is why the name comes off `LineRecord` rather than out of `session.menu`, which is where `CounterOrderLine` legitimately gets it — a screen showing an open order wants today's spelling, a receipt for a past sale wants the one the guest agreed to.

4. **A voided line does not print.** `class_bases` skips it, so printing it would give a document whose rows do not sum to its own subtotal — worse than a terse one. A void is recorded in the log, where an auditor looks for it, and ADR-0115's reason code is what makes it answerable there.

**Consequences accepted.**

- **A long bill prints a long receipt.** That is what a receipt is. No truncation, no "…and 12 more": a document that omits lines to save paper is not the document any of the rules above ask for.
- **The rows are computed at settle and carried on `BillView`**, edge-local beside `totals`, and cross no wire. The receipt is composed after the transaction commits (ADR-0100), so the lines have to survive the commit boundary in hand rather than be re-read from a projection that the settle has already moved on.

**Deliberately not decided here.** Three gaps are named rather than papered over, because each needs something this record cannot supply:

- **Unit of measure is not printed, because the catalog has not got one.** Decree 123 Art. 10 asks for đơn vị tính. There is no unit field on a `CatalogItem`, and printing "each" at settle time would be the till asserting something the operator never said — on a document that is evidence. It needs a catalog field and a publish path first, and it is the one item on this list that a Vietnamese audit could actually turn on.
- **The chosen modifiers do not print.** Their prices are already inside `unit_price` (the event says so where the field is declared), so the arithmetic is right and the guest is charged correctly. But only their **ids** were captured at add time, and resolving the names from the live menu would break decision 3 above. Capturing the names is an event-schema change and belongs with the same gap on the till's line list and the kitchen board.
- **There is no per-line tax rate column.** The tax block already prints a line per rate with its named parts (ADR-0104), and every line's class is inside one of those sums — a store selling at two rates gets both rates and both amounts. What it does not get is which row fell under which rate. That is enough for Vietnam and Japan as this document is used; a market that demands the per-line rate gets the column when that market is entered, and will want the per-line tax *amount* with it.
