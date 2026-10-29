---
kind: added
---
- **Any cell says which cell it is and where a tenant lives.** `GET /.well-known/pos-cell` is public
  and answers from memory with this cell's id, base URL, country alias, home country and directory
  version; with `?tenant=<slug>` it adds the tenant's home cell, that cell's base URL and whether it
  is this one (ADR-0179 C3). An unlisted tenant is a `404`, a malformed slug a `400`, and a cell
  configured for a signed directory that holds none yet answers `503` for a tenant. It never
  redirects, and an answer may be cached for 60 seconds. It is in `docs/openapi-device.json`, where a
  box being set up finds it.
