---
kind: added
---
- **The console's OpenAPI document gives the catalog's answers and request bodies their schemas.**
  The catalog, menus, items, placements, layout buttons, tax classes and rates, translations,
  media and the fee preview name their success and request bodies in `docs/openapi-admin.json`,
  and `dashboard/src/api/generated/admin.d.ts` is regenerated from it. A list that pages is a
  `oneOf` of the whole array and the page. The fee routes that answer ad-hoc JSON wait for their
  typed bodies.
