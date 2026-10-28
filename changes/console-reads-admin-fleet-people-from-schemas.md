---
kind: changed
---
- **The console's admin, fleet, OTA, audit, people, floor, kitchen and alert types come from the
  OpenAPI document.** Forty-one more hand-mirrored types in `dashboard/src/api/types.ts` are
  aliases in `dashboard/src/api/schemas.ts`. An alert's severity a newer cloud names is shown as
  its token and sorts below the ones the console knows.
