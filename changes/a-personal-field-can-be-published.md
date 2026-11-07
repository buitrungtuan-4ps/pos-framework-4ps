---
kind: added
upgrade_note: The cloud and the edge no longer refuse a field declared `personal: true`, and the `PERSONAL_FIELD_UNAVAILABLE` refusal code is gone; a personal field that breaks a rule is refused with `INVALID`, as every other fault is. A store's box must run with full-disk encryption before its package declares a personal field (go-live step 21, gate L8). Rollback is safe for the code, but an older edge refuses a package with a personal field whole and keeps its previous package.
---
- **A personal field can be published** (ADR-0206 decision 11). Each personal record now expires
  on its own period and an erasure reaches the store that holds it, so a package may declare a
  field `personal: true`, with its retention period and consent purpose. The value is entered at
  the till behind its consent, stays on the store, and is never in an event or a log. Go-live gains
  a gate: a box that holds personal custom values runs with full-disk encryption.
