---
kind: added
upgrade_note: The edge applies the `vendors` node. A delivery order from a vendor with `staff_confirm` now waits in the guests' queue for a member of staff, and the store posts accept, reject and ready answers to POST /sync/stores/{store_id}/delivery/answers by `order_id`. The waiting-orders answer (GET /api/orders/awaiting-confirmation) gains an optional `vendor_name`; `table_id` is empty for a delivery order.
---
- **A store takes delivery orders as its vendors say.** An order from a vendor whose orders staff
  confirm waits in the guests' queue under the vendor's name; the store accepts it with its
  vendor when it takes it, rejects it with the reason when staff refuse it, and says it is ready
  when the kitchen has made it (ADR-0215).
