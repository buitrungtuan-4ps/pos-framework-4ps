---
kind: added
---
- **Delivery apps are reconciled with their commission.** A delivery connection may set
  `commission_bps`; the cloud keeps each vendor's settled orders and gross per day, and
  `GET /admin/stores/{store_id}/delivery/reconciliation` and the console's Reports screen show the
  orders, gross, commission at the rate and the net to be paid out. Bills stay gross (ADR-0248
  decision 3).
