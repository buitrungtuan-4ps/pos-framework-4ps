---
kind: added
---
- **The console stages a change to one group of settings and publishes it to one place** (ADR-0246
  decision 3). On **Settings history**, choose a group (such as `einvoice`), set the values to
  change, then choose where they apply: every store, a brand, a store group or one store. Nothing
  reaches a store until **Publish**, and then only the stores that place takes in, each value through
  the ordinary audited setting write. A setting the place cannot hold is skipped and named; a value
  that needs a recorded confirmation is set on the Settings screen.
