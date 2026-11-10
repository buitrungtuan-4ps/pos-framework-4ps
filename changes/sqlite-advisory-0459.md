---
kind: security
---
- **The edge's bundled SQLite no longer returns uninitialised page-cache memory for a tampered `-wal` file.**
  `rusqlite` 0.41 bundles a SQLite fixed for CVE-2026-119375 (RUSTSEC-2026-0459).
