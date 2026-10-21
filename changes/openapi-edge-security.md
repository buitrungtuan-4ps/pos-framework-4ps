---
kind: added
---
- **The till's OpenAPI document (`docs/openapi-edge.json`) now says who may call each route**
  (ADR-0177 decision 5). It declares `till_session`, the token a paired device presents, and every
  operation requires it, except the five that are public (`/healthz`, pairing, activation and its
  standing, and the claim page's status), which carry `security: []` and an `x-public-reason`.
  Each route a signed-in person needs a permission for carries it as `x-permission`, exactly as
  `ROUTE_PERMISSIONS` lists it; tests fail on an operation with neither, and on any difference from
  the table.
