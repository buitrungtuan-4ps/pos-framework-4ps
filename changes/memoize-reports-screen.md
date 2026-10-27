---
kind: changed
---
- **Memoize render calculations in Reports screen.** Wrapped `max`, `count`, `drawerRows`, and `maxProductValue` with `createMemo` in the console Reports screen to avoid repeated $O(N^2)$ computations in JSX render loops.
