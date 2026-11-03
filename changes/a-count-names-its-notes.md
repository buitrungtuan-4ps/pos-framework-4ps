---
kind: added
upgrade_note: `cash.shift.opened` and `cash.shift.counted` gain optional `denomination_counts` and `uncounted_amount` (ADR-0186); both stay at `schema_version` 1 and `PROTOCOL_VERSION` stays 1. `POST /api/shifts` and `POST /api/shifts/{id}/count` accept them and answer `422 COUNT_DOES_NOT_ADD_UP` when they do not add up to the amount. An older edge ignores them, and an older till sends none.
---
- **A float and a count can be recorded note by note**
  ([ADR-0186](../docs/adr/0186-a-drawer-is-counted-by-denomination.md)). The open and count requests
  take each note and coin with its quantity, plus anything the list does not name, and the edge
  refuses a breakdown that does not add up to the amount. The printed shift report lists the
  count's notes under its figures, and the close card reads them. The till's counter that sends them
  comes separately.
