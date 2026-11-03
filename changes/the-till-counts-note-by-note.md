---
kind: added
---
- **The till counts a drawer note by note**
  ([ADR-0186](../docs/adr/0186-a-drawer-is-counted-by-denomination.md) decision 3). Where the store
  has a counting list (`cash_count_denominations`), the Shift screen's opening float, count and drop
  to the safe are a counter: a row per note and coin, largest first, with − and + keys, the keypad
  into the row last tapped, and the row's value; then **Other / loose** (*Khác / lẻ*); then the
  running total in large type. The total is what is recorded, and the rows go with it, so the edge
  keeps the breakdown and the close card lists it. **Drop to safe** now opens the pile to drop, then
  one tap records it. A store with no counting list keeps the single amount field and its note keys.
