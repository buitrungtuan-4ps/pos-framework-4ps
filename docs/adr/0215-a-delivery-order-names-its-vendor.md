# ADR-0215 — A delivery order names its vendor, and a store answers it through the cloud

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-13
· Extends [ADR-0083](0083-integration-doctrine.md) decision 3 (connectors) and
[ADR-0198](0198-a-store-reaches-the-internet-only-through-the-cloud.md) (outbound vendor calls) ·
Relates to [ADR-0056](0056-public-order-intake.md), [ADR-0061](0061-order-relay.md),
[ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md) (Proposed),
[ADR-0194](0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)

## Context

Roadmap-v3 B6.3 and B6.4 are in phase 5. A connector on the cloud can already turn a vendor's
webhook into `/v1/orders` (ADR-0083 decision 3), keyed `(sales_channel, external_reference)` and
carrying no vendor. What remains changes contracts this repository holds to an ADR:

- **The vendor on the order.** `InboundOrder` (a port, `pos-ports`) has no `vendor_id`, so a store
  cannot tell two marketplaces apart, and two vendors' references can collide.
- **The delivery address.** It is personal data (Decree 13/2023, GDPR). ADR-0194 keeps personal
  fields out of events and logs, apart, with a retention period; an order event cannot carry it.
- **The store's answer.** Accept, reject, ready and busy go to the vendor through the
  `DeliveryVendor` port, which the edge calls zero times. ADR-0198 says a store calls out only
  through the cloud, so the port's adapter is the cloud's, and the edge needs a store-facing
  route to reach it.
- **A live `vendors` node** telling the edge which vendors a store takes, and whether staff confirm
  each order before the kitchen sees it (`pos-proto`).

## Decision

1. **`InboundOrder.vendor_id`**: an optional opaque id, the tenant's delivery connection id,
   carried into `sales.order.opened` as an additive field. Idempotency stays
   `(sales_channel, external_reference)`; a connector prefixes the reference with its connection,
   so two vendors cannot collide.
2. **The item map is the connection's data**: external item id to `menu_item_id`, in the
   connection's settings; an unmapped item refuses the whole order back to the vendor.
3. **The address is personal data**, stored on the cloud beside the order relay as a personal
   record (ADR-0194) with the store's retention, reaching the edge as a subject record and never
   in an event or a log; printed on the delivery slip only.
4. **The store answers through the cloud.** A store-facing route
   `POST /sync/stores/{store_id}/delivery/answers` (scope `relay_orders`) takes accept, reject with
   a reason code, ready and busy; the cloud calls the vendor through its connector, with
   ADR-0198's guard, deadlines and idempotency.
5. **A `vendors` node** per store: the connections it takes, and `staff_confirm` per vendor
   (orders wait in the guests' queue for a person, as QR orders do, ADR-0012).
6. **No vendor is special.** Nothing in `pos-core`, `pos-proto`, the edge or the till names a
   particular vendor: a vendor is a connection whose provider has a cloud connector (the
   `OrderConnector` seam), and the store sees only an opaque `vendor_id`, the generic answer codes
   and the `vendors` node. Adding a vendor in any country is one cloud connector and its provider
   entry, with no store release and no protocol change (the owner's decision, 2026-11-13: any
   vendor can be integrated later).

## Consequences accepted

- A port change and additive `pos-proto` and event changes, each with the owner's review.
- A personal-data record on the cloud with a retention period, and its DPIA note.
- A vendor whose protocol the generic answer codes cannot express needs a new ADR, not a branch in the core.
- ADR-0153 stays Proposed for its provider catalogue; this decides only the delivery half.
