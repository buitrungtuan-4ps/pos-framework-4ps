---
kind: added
upgrade_note: "`sales.order.opened` gains two optional fields, `order_type` and `sale_source` (ADR-0260), left off the wire while absent. Older orders read exactly as before. A reader on an older release ignores them and reads such an order as ADR-0259 derives it."
---
- **An order can record its order type and its sale source.** `sales.order.opened` carries an
  optional `ORDER_TYPE_*` and `SALE_SOURCE_*` token, and the till's X and the console's channel lines
  report by the recorded token where there is one (ADR-0260).
