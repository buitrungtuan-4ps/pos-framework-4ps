---
kind: changed
---
- **A tap's change reaches the screen before its request leaves.** Every edge write the till paints
  first now sends its request once the frame showing the change is produced, and the menu bar
  scrolls the current screen into view after the new screen is drawn, not inside the tap
  (ADR-0208). A change painted just before the page reloads or closes is still sent. Adding a line
  and changing a quantity paint about a quarter faster at the median on an emulated slow till.
