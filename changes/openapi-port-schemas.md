---
kind: added
---
- **The print-job claim and the configuration pull name their schemas.** `pos-ports` gains an
  off-by-default `openapi` feature (ADR-0180) that gives the print job and the configuration update
  their schemas, so `GET /api/print/jobs` in `docs/openapi-edge.json` and
  `GET /sync/stores/{store_id}/config` in `docs/openapi-device.json` name their answers. A store's
  default build links nothing new. The till's document now has a schema for every body.
