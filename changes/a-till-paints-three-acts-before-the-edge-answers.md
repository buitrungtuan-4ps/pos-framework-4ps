---
kind: changed
---
- **Adding a line, changing a quantity and moving a table show on the tap** (ADR-0173).
  - **Sending:** the till paints the change at once. A new or changed line shows **Sending…** until
    the store confirms it, and **Take payment** waits, saying why.
  - **Refused:** the change is taken back, with the store's own sentence and no line left behind,
    as for an item that has just sold out or a bill another device has just opened. A refused move
    brings the guests back to their table.
  - **Always waits:** payment, voids, discounts, refunds, comps, sending to the kitchen and every
    drawer act.
