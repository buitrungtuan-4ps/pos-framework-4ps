---
kind: changed
---
- **The cloud's reporting routes have a module of their own** (ADR-0170 decision 4, the reporting
  pilot). The daily rollup reads and their export, the revenue read with its totals, fees and tax
  exports, and the X/Z report move out of `crates/pos-cloud/src/http.rs` into
  `crates/pos-cloud/src/reports/`. Nothing a caller sees changes: every route, permission and
  OpenAPI entry is the same.
