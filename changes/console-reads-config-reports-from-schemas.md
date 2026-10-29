---
kind: changed
---
- **The console's config, key, device and report types come from the OpenAPI document.**
  `dashboard/src/api/schemas.ts` names the generated schemas; twenty-one hand-mirrored types in
  `dashboard/src/api/types.ts` (the config versions and nodes, enrolment, API keys, device
  proposals, terminals, webhooks, the daily rollup, revenue and cash, and the translation import)
  are now aliases of them. A printer's paper a newer cloud names is shown as its token, not as
  80 mm paper, and is kept when the printer is saved. In `docs/openapi-admin.json` a day's
  `by_fee`, `by_drawer` and an X/Z report's `till_names`, always sent, are required.
