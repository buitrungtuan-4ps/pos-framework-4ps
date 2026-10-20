---
kind: changed
---
- **The Menus sub-screen memoizes dropdown options.** Option object allocations for items, sections, currencies, and menus in placement/publish drawers are memoized with `createMemo` instead of mapping arrays inline on every render pass.
