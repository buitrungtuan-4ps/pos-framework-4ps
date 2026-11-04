---
kind: fixed
---
- **A counter order whose first open fails no longer blocks the draft.** If the store server refuses
  the open, the next tap on an item tries again. If it does not answer, the till says the order is
  not confirmed and opens no second order from that draft; the operator goes back and starts the
  order again.
