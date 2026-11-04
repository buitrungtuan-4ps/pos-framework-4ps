---
kind: added
---
- **A report counts custom values.** `GET /admin/stores/{store_id}/custom-values/summary` and its
  `summary.csv` say how many of a store's orders, lines, bills or tables, or records of an object
  type, hold each value of one custom field. A filter can keep only the objects whose second field
  holds one value. The report needs `console.data.read`. It refuses a field any package version
  declares personal with `PERSONAL_FIELD`, and it never names or counts a person (ADR-0194).
