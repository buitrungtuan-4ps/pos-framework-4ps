---
kind: changed
---
- **Every body in the cloud's OpenAPI documents has a schema, and a test holds it there.** The
  emptied `ADMIN_NO_SCHEMA_YET` and `DEVICE_NO_SCHEMA_YET` ratchets are gone: a route in the console,
  store-box or `/v1` document whose answer or request body names no schema now fails
  `openapi_bodies` outright. Nothing on the wire changes.
