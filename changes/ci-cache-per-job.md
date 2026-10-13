---
kind: changed
---
- **CI: each job restores its own cache, saved from `main` only, and builds without debug info
  and links with mold on Linux** (ADR-0171 decision 1). Every job of every pull request used to
  save `target/` under one shared key, so whichever finished first decided what all the others
  restored, and a pull request could overwrite `main`'s cache. Now `Swatinem/rust-cache` keys a
  cache per job, OS and build environment.
  - Only a run on `main` saves. `pr.yml` now also runs on a push to `main` to do that, and pull
    requests only restore.
  - CI sets `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_PROFILE_TEST_DEBUG=0`,
    the same in `pr.yml` and `main.yml`.
  - Linux jobs link with mold 2.42.1, checked against the sha256 in `tools/tool-versions.toml`.
  - Job names are unchanged. Nothing a store or a developer's own build runs changes.
