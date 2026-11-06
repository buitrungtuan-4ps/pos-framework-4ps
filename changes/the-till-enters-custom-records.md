---
kind: added
---
- **The till enters custom records.** A store whose package declares an object type, such as a
  reservation, gets a Records screen: each type's records, and a form that enters or changes one
  with each field in the form its type takes and its label from the tenant's grid. A personal
  value is sent only with its consent ticked and is never shown again unless the person holds
  `data.personal.read` (ADR-0194, ADR-0206). `GET /api/package` answers the running version's data
  part as `running.data`.
