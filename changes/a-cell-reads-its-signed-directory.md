---
kind: added
---
- **A cell reads its signed cell directory.** `[cell] directory_path` and `directory_public_key`,
  set together or not at all, name the directory file and the public half of the directory key
  (ADR-0179 C2). The cell reads the file at boot and every five minutes, and takes a new directory
  only when its signature, content and version check out. A file that fails is refused, the reason is
  shown on the Fleet screen's cell panel, and the directory already held is kept. A cell with the key
  and no accepted directory answers for no tenant. Without the two keys a cell answers for itself, as
  before. `GET /admin/cell` gains `directory_mode`, `directory_version` and `directory_refusal`.
