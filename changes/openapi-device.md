---
kind: added
---
- **A fourth OpenAPI document, `docs/openapi-device.json`, describes what a store box calls on the
  cloud** (ADR-0177): the `/sync` surface (configuration and heartbeat, events and their hello,
  reconciliation, the order relay, update artifacts and reports, archives and their key, device
  proposals) and the two ways a box first comes to hold a credential, `/activate` and `/claim`, 16
  operations in all. A `pos-cloud` test fails on a `/sync`, `/activate` or `/claim` route that is
  not documented, and on a documented path no router serves.
