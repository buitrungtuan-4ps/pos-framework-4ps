---
kind: changed
---
- **The console's catalog, capability, integration, settings and fee report types come from the
  OpenAPI document.** Thirty-three more hand-mirrored types in `dashboard/src/api/types.ts`
  (webhooks, the X/Z report, item and display categories, layout buttons, modifier groups, items,
  menus, sections, placements and channel prices, the signed-in administrator, capability flags,
  presets, rules and catalogue, integration providers and connections, setting definitions,
  values, effective values and publish reports, and the fee publish reports, faults and sample
  bill) are aliases in `dashboard/src/api/schemas.ts`. An integration provider's family is typed
  as the open token the cloud sends.
