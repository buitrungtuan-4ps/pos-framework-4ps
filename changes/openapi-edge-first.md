---
kind: added
---
- **The till has an OpenAPI document, `docs/openapi-edge.json`** (ADR-0177). It starts with
  `/healthz`, the `/ws` fan-out and pairing. Every other edge route is named in a coverage-debt list
  that only shrinks, and a test fails a route that is in neither, a documented path no router
  serves, and an `operationId` used twice. `pos-edge` gains an `openapi` feature, off by default and
  turned on only by its own tests, so a store's binary compiles none of it: the release binary
  measured 15,174,264 bytes with and without the annotations. Two handlers are renamed for clear,
  unique `operationId`s, with no route changed.
