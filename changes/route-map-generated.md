---
kind: added
---
- **`docs/generated/route-map.md` lists every route the four OpenAPI documents describe** (ADR-0177,
  P11 item 5): 361 lines, one per operation, with the method and path, the handler (its
  `operationId`), who may call it, the permission it needs and the file the handler is in.
  `cargo xtask gen-docs` writes it from the committed documents, and `--check` fails when it
  differs, so a document regenerated without the map fails `just preflight`. A handler the map
  cannot find, or finds twice, stops the render.
