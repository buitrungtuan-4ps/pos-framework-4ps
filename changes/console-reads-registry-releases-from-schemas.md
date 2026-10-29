---
kind: changed
---
- **The console's registry, release, inventory and country types come from the OpenAPI
  document.** Twenty-two more hand-mirrored types in `dashboard/src/api/types.ts` (tenants,
  brands, stores, store groups and batch reports, devices, money, tax classes, rates and
  components, activation codes, scheduled publishes, releases, previews, countries, quantities,
  ingredients and suppliers) are aliases in `dashboard/src/api/schemas.ts`. An ingredient's unit a
  newer cloud names is shown as its token and kept when the ingredient is saved. `types.ts` is now
  under the console's file-size limit, so its baseline entry is removed.
