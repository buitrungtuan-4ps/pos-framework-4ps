---
kind: added
upgrade_note: PUT /admin/config/integrations also publishes a `vendors` node (pos_proto::vendors) and answers `vendors`; it refuses (422) a store-scoped delivery connection that has no `staff_confirm` and `prep_minutes` settings, which every delivery provider now requires. POST /sync/stores/{store_id}/delivery/answers takes `order_id` in place of `external_reference`. Migration 0102 adds an index on the order queue.
---
- **A store is told which delivery vendors it takes orders from.** Each vendor's name, whether
  staff confirm its orders, and the minutes an accepted order takes reach the store with its
  integrations; a store can answer a vendor about an order by its own id (ADR-0215).
