---
kind: changed
---
- **Moving a table's guests paints sooner.** The order screen takes the new table in the same
  update as the move, so the order's lines are drawn once, and the list of free tables stays drawn
  while it is closed. On an emulated slow till a move paints about twice as fast at the median.
