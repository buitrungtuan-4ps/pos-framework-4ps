---
kind: added
---
- **The store-box and `/v1` OpenAPI documents now say who may call each route** (ADR-0177
  decision 5). `docs/openapi-device.json` declares `device_credential`, and each `/sync` route
  requires it with the scope it checks as the requirement's role; `/activate`, `/claim` and its
  collection are public, `security: []`, each with an `x-public-reason`. In `docs/openapi.json`,
  and the `/v1/openapi.json` the cloud serves, each route's `api_key` requirement now names its
  scope (`read_rollups` or `place_orders`). Tests fail on an operation that says neither, and on
  a role that is not a scope.
