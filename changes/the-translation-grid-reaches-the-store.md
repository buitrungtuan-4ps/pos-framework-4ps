---
kind: added
---
- **A store holds its tenant's translation grid.** The edge pulls the grid whole from the new
  `GET /sync/stores/{store_id}/translations` beside its configuration, keeps a copy on disk for an
  offline restart, and `GET /api/package` answers the labels of the keys the running package
  names as `running.labels`. The till shows a package's labels, such as a custom field's name, in
  its language and then English, and never shows a raw key (ADR-0043).
