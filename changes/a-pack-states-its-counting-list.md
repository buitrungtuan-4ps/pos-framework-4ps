---
kind: added
upgrade_note: The `locale` node gains an optional `cash_count_denominations` (ADR-0186). A locale publish that leaves it out takes the country pack's list, so a store's node carries Vietnam's 1,000 ₫ to 500,000 ₫ after its next locale publish; until then the till has no counting list, as before. An empty list means no counter. Older edges ignore the field.
---
- **A country pack states the notes and coins a drawer is counted by, and a store can change the
  list** ([ADR-0186](../docs/adr/0186-a-drawer-is-counted-by-denomination.md)). Vietnam counts 1,000 ₫
  to 500,000 ₫, Japan ¥1 to ¥10,000 and India ₹1 to ₹500; the reference country states none. Store
  settings shows the list beside the quick-cash notes, filled from the country, and the till reads
  it as `cash_count_denominations` on `GET /api/locale`. The quick-cash notes keep their job on the
  Pay screen.
