---
kind: added
upgrade_note: The event `sales.order.opened` gains an optional `package_version_id` (ADR-0192 decision 5). It is additive and omitted while absent, so the schema version stays 1, old orders replay unchanged, and a reader that does not know the field ignores it; a consumer that rejects unknown payload fields must accept it before the edge starts writing it.
---
- **`sales.order.opened` can record the package version the order opened under.** An optional
  `package_version_id`, left off the wire while absent, so every order written before it, and
  every order opened under the default package, keeps its bytes (ADR-0192 decision 5). The edge
  writes none yet: it starts once a store applies a pinned package version.
