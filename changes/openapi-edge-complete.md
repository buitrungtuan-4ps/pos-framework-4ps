---
kind: added
---
- **The till's OpenAPI document (`docs/openapi-edge.json`) now describes every edge route**
  (ADR-0177). The last 11 are activation and its standing, the claim page's status, minting a
  pairing code and retiring a device, binding and releasing a print agent and its terminal list,
  the agent's job claim and acknowledgement, and a printer's test page. The coverage-debt list is
  gone: an edge route added without an annotation now fails the build's tests. Eleven handlers are
  renamed so each `operationId` is unique and says what it does, with no route changed.
