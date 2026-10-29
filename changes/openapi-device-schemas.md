---
kind: added
---
- **The store box's OpenAPI document gives its sync routes their schemas.** The heartbeat, event
  publish and protocol hello, the archive and its key, the OTA artifact and report, and the queued
  order pull and acknowledgement name their success and request bodies in
  `docs/openapi-device.json`. The archive upload is now described as `application/octet-stream`,
  which is what the box sends, rather than JSON.
