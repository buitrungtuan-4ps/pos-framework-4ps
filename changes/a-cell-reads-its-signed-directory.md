---
kind: added
---
- **A cell reads a signed cell directory.** `[cell] directory_path` and `directory_public_key`
  name one JSON file, the same in every cell, mapping tenant slugs, countries (with a `"*"`
  fallback) and cell ids to cells and their addresses (ADR-0179 C2). The cell checks its minisign
  signature against the directory key, its content and its version at boot and every five minutes.
  A file that fails is refused, the reason is shown on the Fleet screen's cell panel, and the
  directory already held is kept. Without the two keys a cell answers for itself, as before.
  `GET /admin/cell` gains `directory_mode`, `directory_version` and `directory_refusal`.
