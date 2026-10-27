---
kind: added
---
- **A store whose country differs from its cell's is warned about, and the warning can be
  answered.** `GET /admin/stores/{store_id}/cell-standing` (`console.data.read`) compares the country
  a store publishes with its cell's home country (ADR-0179 C6). Nothing is refused for a difference.
  A person with `console.stores.manage` answers it with one sentence through
  `POST /admin/stores/{store_id}/cell-acknowledgement`, recorded in the audit trail as
  `store.cell_country.acknowledge`. The answer stands only while the store and its cell still have
  the two countries it named. No table or migration is added: the audit entry is the record.
