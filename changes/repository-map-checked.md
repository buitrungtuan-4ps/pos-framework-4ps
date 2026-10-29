---
kind: changed
---
- **`docs/repository-map.md` is refreshed and checked** (P11 item 5). It now names where an edge
  route is registered (`http/guarded.rs`), the OpenAPI annotation, document, route map and
  generated TS types every route has (ADR-0177) in place of the retired "phase 8 makes it one"
  promise, and how a generated document is added. `cargo xtask repo-map`, now in
  `just preflight`, fails when a path the map names in backticks does not exist; a path with a
  placeholder or wildcard names the directory it starts in.
