---
kind: added
---
- **The console's OpenAPI document names what a config node read answers.** A read whose route
  fixes the node (`channels`, `tender`, `origins`, `qr`, `retention`, `vendors`) names the node's
  wire type, or `null` when the store has none. The effective store configuration, a version's
  document, the OTA rollout and placement reads and `/admin/openapi.json` are described as
  free-form JSON, with each route saying what the JSON is. The OTA installer is described as bytes.
