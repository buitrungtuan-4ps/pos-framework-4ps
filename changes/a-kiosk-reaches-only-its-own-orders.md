---
kind: security
---
- **A kiosk reaches only self-order's routes, and only its own orders** (ADR-0224 decision 4).
  A kiosk's own record may read the menu, locale, layout, package and reason list, open a
  tableless order, and read, ring, change, void, send to be paid and close an order it opened
  itself. Every other route, reads included, answers `403 KIOSK_NOT_ALLOWED`, so a kiosk cannot
  list the store's orders, act on another till's, or reach money, the drawer or a shift.
