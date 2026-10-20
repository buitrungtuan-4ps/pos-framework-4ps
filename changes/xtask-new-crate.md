---
kind: added
---
- **`cargo xtask new crate` writes a workspace crate that the checks accept as written**
  (ADR-0176). It writes four things:
  - the manifest, with a complete contract card whose owners come from `CODEOWNERS`;
  - a README with the six headings and the lines `xtask crates` holds;
  - the crate root;
  - its line in the workspace `members`.

  It refuses a file that already exists, and `--dry-run` prints the files without writing them. It
  is the base the adapter and country generators build on.
