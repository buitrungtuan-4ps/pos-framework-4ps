---
kind: changed
---
- **Generated files are marked in `.gitattributes`** (ADR-0170 decision 5), so a pull request's
  diff collapses them and the language statistics leave them out. The marked files are:
  - the snapshots under `docs/snapshots/`;
  - both OpenAPI documents;
  - `docs/configuration.md`, `docs/permissions.md` and `docs/state-machines.md`;
  - the two generated Windows installer templates;
  - the lockfiles.

  The file names the command that regenerates each. Nothing they contain changes.
