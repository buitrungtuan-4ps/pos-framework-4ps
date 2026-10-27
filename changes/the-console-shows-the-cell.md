---
kind: added
---
- **The console shows which cell it is.** `GET /admin/cell` (`console.data.read`) reads the
  `[cell]` section of `cloud.toml` (ADR-0179 C1), and the Fleet screen shows it in a small panel: the
  cell id, country alias, home country, the country its servers run in and its address. A cloud with
  no `[cell]` section says it is the single cell. When the servers run outside the home country the
  panel warns, and refuses nothing.
