---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, and the `/admin/openapi.json` the
  cloud serves) now says who may call each route and what permission it needs** (ADR-0177
  decision 5). Every operation requires the `session_cookie`, the 65 that did not say so included,
  except four public ones (sign-in, first-owner setup, accepting an invitation and the document
  itself), which carry `security: []` and an `x-public-reason`. The 254 operations whose handler
  checks a console permission carry its id as `x-permission`. Tests fail on an operation that says
  neither, and on an `x-permission` that differs from the handler's `require_permission` call.
