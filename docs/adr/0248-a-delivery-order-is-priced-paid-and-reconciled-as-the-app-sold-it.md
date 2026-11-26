# ADR-0248 — A delivery order is priced, paid and reconciled as the vendor's app sold it

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 (open item O6) · Extends
[ADR-0215](0215-a-delivery-order-names-its-vendor.md) · Relates to
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0225](0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md)

Every statement of law or accounting below is **to be confirmed by the accountant**.

## Context

A delivery guest orders and pays in the vendor's app. The store reprices the order at its own menu,
answers the vendor with the total before tax (217,000 ₫ where the store charges 236,000 ₫), has no
tender for "paid in the app", records no commission, and shows the order like any other.

## Decision

1. **Price source, per vendor connection:** `price_source`, `VENDOR_PRICE_SOURCE_APP` (the
   recommended default: the line is priced at the app's quoted price, so the bill matches what the
   guest paid) or `VENDOR_PRICE_SOURCE_MENU` (the store's delivery menu). The order is flagged when
   the two differ by more than `price_difference_bps` (basis points, default `0`: any difference), and
   the till shows the flag on the order.
2. **Paid via the vendor.** A new payment method, `PAYMENT_METHOD_VENDOR` (additive), "Paid via
   <vendor>". Where the connection says `payment` is `VENDOR_PAYMENT_IN_APP`, the edge settles the
   order's bill with it when the order is accepted. `VENDOR_PAYMENT_AT_STORE` leaves the bill to be paid
   at the till, as today.
3. **Commission is recorded apart**: `commission_bps` per connection; the bill and its tax stay
   gross, and the cloud's reconciliation report shows, per vendor and day, the orders, the gross paid,
   the commission at the rate and the net expected.
4. **The answer to the vendor carries the order's total with tax**, the amount the bill will charge.
5. **Badged.** The counter and the KDS show a delivery order's vendor.
6. **The e-invoice** of an app order is issued for the bill, which under `APP` is what the guest paid,
   to be confirmed by the accountant.

## Consequences accepted

- Four fields on the vendor connection, one payment method, one additive field on the answer. The
  defaults change a delivery order's price to the app's: an upgrade note says so.
- Settling at accept means a bill exists before the food does; a rejected order is never accepted, so
  never settled.
