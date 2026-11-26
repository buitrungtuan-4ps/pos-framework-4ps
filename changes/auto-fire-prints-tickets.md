---
kind: fixed
---
- **An order sent to the kitchen on arrival prints its kitchen tickets** (ADR-0247). Under
  `KITCHEN_RELEASE_AUTO_ACCEPT_AND_FIRE` a QR or delivery order's lines reached the kitchen screen but
  printed no ticket, so a kitchen with printers and no screen never saw it. The store now prints a
  ticket for each line it sent as it takes the order, once.
