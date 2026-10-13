---
kind: changed
---
- **A pull request runs only the CI jobs its change can affect** (ADR-0171 decision 2).
  - A first job, `changes`, sorts the changed files with `cargo xtask affected --classes` into
    `rust`, `rust_all`, `ui`, `dashboard`, `shared`, `deploy`, `workflows` and `docs_only`. Each
    job runs only for the classes it depends on. A docs-only pull request runs just `rules`,
    `secrets` and `docs-gate`.
  - The core crates, the lockfile, toolchain and lint configs, `.github/`, `xtask/` and `tools/`
    run all of Rust. An unknown file runs everything, and so does a push to `main`.
  - No job is renamed, and a skipped job reports success to a required check. If `changes`
    fails, every job runs.
