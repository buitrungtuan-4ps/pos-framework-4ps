---
kind: fixed
---
- **The X/Z drawer table sets the float of closed shifts beside the expected amount.** The cash
  rollup adds `closed_float` to each day's and each drawer's cash figures: the float of the shifts
  that closed, on the day of the close. A drawer whose second shift is still open no longer reads
  as two floats beside one shift's expected amount. `opening_float` keeps its meaning and its
  column, *Floats opened*. A shift opened before this release adds no closed float until the
  rollup is reset.
