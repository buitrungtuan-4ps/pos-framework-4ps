---
kind: added
upgrade_note: Migration 0099 adds the gateway_calls table (answers kept 24 hours). New API-key scope call_gateway; no existing key or device credential gains it.
---
- **A store can make an external call through the cloud.** `POST /sync/stores/{store_id}/gateway/calls`
  calls one of the tenant's gateway endpoints with the store's values: checked against the
  endpoint's fields, sent behind the webhook guard within the endpoint's deadline, retried only when
  the endpoint is idempotent, limited to ten calls a second per store and fifty per endpoint, and
  paused while the endpoint keeps failing. A repeated idempotency key within 24 hours gets the first
  answer back. A store key needs the new `call_gateway` scope, which the console's API-key screen
  now offers.
