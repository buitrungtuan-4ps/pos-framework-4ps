---
kind: fixed
---
- **The product mix counts a line's changed quantity.** The cloud's revenue rollup took each item's
  ordered quantity and value from the line as it was added and ignored a later change, so an iced
  tea added at one for 39,000 and raised to two showed 39,000. A line's update now replaces what it
  added, on the day it was added. A rollup stored before this keeps no lines, so an update to a line
  added before then changes nothing until the rollup is reset.
