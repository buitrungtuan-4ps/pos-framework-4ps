---
kind: added
upgrade_note: sales.order.opened gains an optional `subject_id` (additive; absent for every order but a delivery order naming its recipient). The edge's relay reads GET /sync/stores/{store_id}/subjects/{subject_id} for such an order before making it, and leaves the order un-acked while the cloud cannot be reached. POST /api/orders/{id}/confirm now prints a confirmed delivery order's slip.
---
- **A delivery order's slip prints its recipient, once.** The store keeps the recipient the cloud
  kept with the order that names it, under the same expiry, and prints the delivery slip on its
  receipt printer when it takes the order: at once, or when staff confirm it (ADR-0215).
