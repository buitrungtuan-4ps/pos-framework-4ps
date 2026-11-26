---
kind: added
---
- **A store records HQ's decision on its correction request** (ADR-0250). Each minute the edge
  records `fiscal.correction.decided` once for each decision its `einvoice` node holds on a bill it
  asked HQ about, stamped by the box. A restart records none twice, and a later decision on the same
  bill is recorded too, so the store's log shows who decided and when, by id.
