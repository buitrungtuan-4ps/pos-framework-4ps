---
kind: added
---
- **The console lists package versions and runs one on every store** (ADR-0189). *Packages ›
  Versions and pins* shows each package's published versions and what every store runs, and
  **Run on every store** pins a version for the whole tenant after a confirmation; running an
  earlier version the same way is the rollback, and **Remove the pin** returns every store to its
  release's default package.
