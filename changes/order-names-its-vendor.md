---
kind: added
upgrade_note: Protocol and event, additive. The OrderIn port's InboundOrder gains `vendor_id` (an opaque VendorId, the tenant's delivery connection), the relayed order payload gains `vendor_id`, and `sales.order.opened` gains `vendor_id`, absent for every other order and left off the wire then, so older events replay unchanged. An edge older than this release ignores the field.
---
- **A delivery order names its vendor.** An order a delivery connector relays carries the
  connection's id as an opaque `vendor_id`, which the store records on the order it opens; no
  vendor is named anywhere a store runs (ADR-0215).
