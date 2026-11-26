---
kind: added
upgrade_note: A delivery connection's new `price_source` defaults to `VENDOR_PRICE_SOURCE_APP`. A vendor that sends a price for a line now has that line sold at the app's price rather than the store's delivery menu; set `price_source` to `VENDOR_PRICE_SOURCE_MENU` on the connection to keep the menu's price. The only connector today, the sandbox `delivery.demo`, sends prices only when its order carries `currency` and `unit_price`.
---
- **A delivery order is priced as the app sold it, or at the menu, per connection.** Each delivery
  connection chooses `price_source` (the app's price by default, or the store's menu) and
  `price_difference_bps`, the tolerance past which the order is flagged; the till warns staff on a
  flagged order before they accept it (ADR-0248 decision 1).
