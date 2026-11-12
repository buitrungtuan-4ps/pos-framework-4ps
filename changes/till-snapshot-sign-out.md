---
kind: fixed
---
- **Signing out leaves nothing of the store's configuration in the till's snapshot** (ADR-0173
  decision 3). A read answered while the sign-out was clearing the snapshot could write its record
  back after the clear; an answer to a read made before the clear is no longer kept.
