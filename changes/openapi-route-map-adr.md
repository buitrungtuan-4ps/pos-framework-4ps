---
kind: added
---
- **ADR-0177: OpenAPI is the route map.** Every edge and cloud route is to be in a generated
  document: the console's and integrators' as now, plus `docs/openapi-edge.json` for the till and
  `docs/openapi-device.json` for what a store box calls. Responses get schemas from the `pos-proto`
  wire types, behind an optional `openapi` feature that is off by default. Each operation's
  `operationId` is its handler's name; standard `security` says what a caller presents, and an
  `x-permission` extension what permission it must hold. The till's and console's TypeScript types
  are to be generated from the documents and drift-checked. The work lands in the pull requests that
  follow.

  Nothing a store or a console user sees changes.
