---
kind: changed
---
- **The devices publish and a pending claim answer typed bodies.** The devices publish answers
  `DevicesPublished`, and collecting a claim nobody has bound yet answers `ClaimPending`, named in
  `docs/openapi-admin.json` and `docs/openapi-device.json`. The bytes on the wire are unchanged.
  Every `/admin` answer and request body now has a schema.
