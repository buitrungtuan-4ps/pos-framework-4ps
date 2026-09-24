# ADR-0144 — A line records the names of its modifiers, so the receipt can print them

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Closes a gap named by [ADR-0129](0129-a-receipt-itemises-what-was-sold.md) · Relates to
[ADR-0127](0127-modifier-groups-reach-the-edge.md), [ADR-0024](0024-protocol-version-negotiation.md)

## The problem

A guest who ordered a 30 cm Margherita gets a receipt that says "Margherita". The modifier's price is
inside the line's unit price, so the money is right, but the receipt no longer says what was bought.
ADR-0129 left this open on purpose. `sales.order_line.added` captures only the modifiers' **ids**, and
printing names from the live price book would break its decision 3: a receipt prints what the guest
agreed to, not today's spelling.

## Options considered

1. **Print names from the current price book.** It is quick, but a rename between ordering and paying,
   or a reprint later, changes a document that is evidence. ADR-0129 rejected this, and the reason
   still holds.
2. **Record the names on the event, beside the ids.**
3. **Keep them off the receipt.**

## Decision

**Option 2.**

- `SalesOrderLineAdded` gains `modifier_display_names: Vec<DisplayName>`, index-aligned with
  `modifier_menu_item_ids`. It is `#[serde(default)]` and additive, so no `PROTOCOL_VERSION` bump
  (ADR-0024).
- Both places a line is added fill it, from the price book the line was priced against:
  - the till's `add_line`;
  - the order intake.
- `LineRecord` carries the names to `ReceiptLine`, and the receipt prints `  + name` under the line,
  as the kitchen ticket already does.
- **A line added before this field existed prints no modifiers.** It prints neither an id nor a
  current name. An old receipt stays what it was.
- The till's order list and the kitchen board keep reading the current price book for an **open**
  order (`docs/ui-ux.md`). They show work in progress, not a past sale.

## Consequences accepted

- **This is a `pos-proto` change**, and it lands with an owner review under ADR-0126.
- **An event grows by the modifier names**, typically one or two short strings. They are menu text,
  not personal data, so they may sit in an event (`pos_proto::pii` has nothing to refuse).
- **Old and new edges interoperate.** An older reader ignores the field; a newer reader of an old
  event sees an empty list.
