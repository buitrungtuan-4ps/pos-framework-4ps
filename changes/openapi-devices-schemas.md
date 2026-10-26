---
kind: added
---
- **The OpenAPI documents give devices and activation their schemas.** Terminals, device proposals
  and their settings, activation codes, claim binding and device revocation name their success and
  request bodies in `docs/openapi-admin.json`, and a store box's `/activate`, `/claim` and
  `/sync/stores/{store_id}/devices` do in `docs/openapi-device.json`. A claim still pending, and the
  devices publish, answer ad-hoc JSON until their typed bodies land.
