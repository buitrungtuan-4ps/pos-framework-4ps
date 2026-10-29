---
kind: added
---
- **The Fleet screen shows the stores an HQ reads in other cells.** A new **HQ: other cells** card,
  under this cell's own panel, lists each store `[federation]` names: its cell, the days read and
  the events counted, or why it could not be read, with a badge counting the stores not read
  (ADR-0179 C10). It shows counts only: **only aggregates cross a cell boundary**, no personal data,
  and no key is ever shown, since none is ever answered. A cell with no `[federation]` section says
  it reads no other cell. The read can take up to 30 seconds, so it is not repeated on tab focus.
