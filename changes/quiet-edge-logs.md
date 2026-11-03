---
kind: fixed
---
- **The cloud log is no longer filled with "there is no transaction in progress".** The cloud's
  database pool ended a transaction with `ROLLBACK` every time it handed a connection out again, and
  PostgreSQL warned on each one that had none, which was 97 % of a pilot's cloud log. A transaction
  dropped before it commits or rolls back now closes its connection instead of returning it to the
  pool, and the pool checks a connection with an empty query, which says nothing.
- **A store archive the cloud has no storage for says so.** With no archive storage configured in
  the cloud, the store server logged that the cloud "could not be reached" while sync worked. It
  now says the cloud keeps no store archives because its archive storage is not configured.
