---
kind: fixed
---
- **A bill voided after payment no longer brings its food back on the kitchen board.** Voiding a
  paid bill (ADR-0184) made its order owe money again, so `GET /api/orders/kitchen` and
  `GET /api/orders/live` listed its fired lines as fresh, late tickets with no open bill, and a
  table seated again since showed them as a counter order. The voided sale has ended as a paid one
  has: it leaves both reads, and a paid counter order still waiting for the kitchen (ADR-0161)
  leaves the board when its bill is voided. A restart replays to the same answer. A bill voided
  before anybody paid it still gives its order back to be billed again.
