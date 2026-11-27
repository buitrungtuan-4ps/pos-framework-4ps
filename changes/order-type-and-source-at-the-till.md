---
kind: added
upgrade_note: "`POST /api/orders` takes optional `order_type` and `sale_source`, `GET /api/menu` adds `order_types` and `sale_sources`, and the X's channel lines add `order_type_names` and `source_names` (ADR-0260), all additive. A kiosk's orders now record `SALE_SOURCE_KIOSK`, so from this release they report as Kiosk rather than the till."
---
- **The till records an order's type and source from the store's lists.** An order type's base
  kind sets the order's channel, price and tax; a kiosk's order is reported as Kiosk; QR, API and
  app orders take the type the store maps their source to (ADR-0260).
