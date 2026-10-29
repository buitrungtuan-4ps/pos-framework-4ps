---
kind: changed
---
- **Every wire type the till reads comes from the OpenAPI document.** The last twenty-seven
  hand-mirrored types (the shift, drawer, batch close and drawer-open answers, the terminals and
  bind answers, paired devices, settled bills and takings, tables and transfers, live orders and
  lines, the menu, fee lines, bills, counter orders, locale and sync, and the void, fee waive and
  settle requests) are aliases in `ui/src/api/schemas.ts`, and `ui/src/api/types.ts` is gone. The
  bytes on the wire are unchanged.
