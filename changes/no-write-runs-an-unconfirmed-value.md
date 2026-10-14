---
kind: added
---
- **Only Settings can turn on a value that needs a recorded confirmation.** A layer write
  (`PUT /admin/stores/{store_id}/config/{level}`), a rollback, or a node publish, single or in a
  batch, that would make a store run such a value, which it did not run before, is refused with
  `400`, naming the setting's key with `CONFIRMATION_REQUIRED` (ADR-0169). A config release is
  checked against each store when it is scheduled and refused the same way, with nothing written.
  A scheduled publish that would do it when it comes due is not applied and records why on its row,
  as a node that no longer validates does. A write that keeps a value the store already runs passes.
  No setting needs confirming yet.
