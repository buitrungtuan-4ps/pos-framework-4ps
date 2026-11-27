# ADR-0259 — A sale is reported by its channel and its order type

**Status** Accepted 2026-11-27 under the owner's request; decision 3 to be confirmed by the owner ·
**Owner** @maintainers-architecture · **Date** 2026-11-27 · Phase 11 ("data matched from edge to
console") · Details [ADR-0187](0187-a-shift-is-handed-over-at-the-till-and-read-there.md) decision 4
and [ADR-0081](0081-reports-and-analytics.md) · Relates to
[ADR-0215](0215-a-delivery-order-names-its-vendor.md), [ADR-0248](0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)

## Context

The owner asks that every sale be read on two axes: **where the order came from** (the store's own
till, a QR table, a kiosk, or a named delivery app such as Grab or Capichi) and **how it is served**
(dine-in, take-away, delivery), the same on the till and the console. Today `SalesChannel` mixes both
axes (`DINE_IN`, `TAKEAWAY`, `DELIVERY`, `QR`, `API`); `sales.order.opened` also carries `table_id`
and, for an app's order, `vendor_id`, whose name lives on the store's published `vendors` node. The
till's X calls money paid in an app "Other tenders"; the console calls it "Paid via delivery apps".

## Options considered

1. **Write both axes on `sales.order.opened` from now on.** Old orders still need a derivation, so
   there would be two sources for one fact.
2. **Derive both axes from the fields the event already carries**, in one function both the edge and
   the cloud call. No event change.
3. **Rename the `SalesChannel` tokens.** Forbidden: published tokens are additive only.

## Decision

1. Option 2. `pos-proto` gains two wire enums and one pure function, `sale_channel`, from an order's
   `channel`, `table_id` and `vendor_id`. Nothing is stored; every order on every log, old ones
   included, is read by the same function on both sides.
2. **Channel** (`SALE_SOURCE_*`): an order naming a `vendor_id` is `SALE_SOURCE_APP` with that
   vendor, shown by the connection's name from the `vendors` node at read time, never a list in
   code (an app the store no longer lists shows its id); else `SALES_CHANNEL_QR` is
   `SALE_SOURCE_QR`, `SALES_CHANNEL_API` is `SALE_SOURCE_API`, and every other order is
   `SALE_SOURCE_POS`, the store's own till, a phone delivery taken by staff included.
   `SALE_SOURCE_KIOSK` is reserved: a kiosk's orders read as `POS` until `sales.order.opened` gains
   the optional `opened_on_kiosk` flag (absent means false), which a later PR may add under this ADR.
3. **Order type** (`ORDER_TYPE_*`), a closed set of three, by how the food leaves the store:
   `DINE_IN` (eaten here), `TAKEAWAY` (the guest carries it), `DELIVERY` (carried to the guest).
   `SALES_CHANNEL_DINE_IN`, `_TAKEAWAY` and `_DELIVERY` map to their own; `_QR` and `_API` are
   `DINE_IN` with a table and `TAKEAWAY` without; anything unknown, and a bill whose order opened
   before `sales.order.opened` existed, is `ORDER_TYPE_UNSPECIFIED` ("not recorded"). **To confirm by
   the owner:** whether named sub-types (pick-up, drive-through) are wanted; they would be published
   configuration mapped onto these three, additively, since reports key types by token.
4. **Reports carry one breakdown at the finest grain**, `by_channel`: one line per channel, app and
   order type, with its bills and sales (`total_due`, what the day's and the shift's sales already
   sum). The lines sum exactly to the bills and sales beside them; a bill voided after payment comes
   out of its line as it comes out of the totals (ADR-0184 decision 9, ADR-0258). The till's X and the
   console's X/Z group the lines by channel and by type, and label a line "Grab · Delivery"
   ("Grab · Giao hàng").
5. **Tenders**: a `PAYMENT_METHOD_VENDOR` payment is named by its app, "Paid via Grab" ("Trả qua
   Grab"), never "other". The till's X adds `tenders.in_app`, per app, and `tenders.other_than_apps`;
   `tenders.other` keeps its meaning (ADR-0111). The console's day adds `by_app_tender`, per app;
   `by_tender` is unchanged. The X/Z report adds `vendor_names` from the `vendors` node, as it adds
   `till_names`.
6. Unchanged: delivery reconciliation per app per day (ADR-0248 decision 3), the product mix, and
   every event, port and route. All labels are translation keys.

## Consequences accepted

- The edge keeps, per open shift, its lines and each paid bill's line so a void can take it out; the
  cloud's rollup keeps each open order's line until it is paid, bounded as the vendor fold's maps are.
  A day folded before this has no lines until the rollup is reset (ADR-0036), with an upgrade note.
- One closed list (order type) lives in code, pending the owner's answer to decision 3.
