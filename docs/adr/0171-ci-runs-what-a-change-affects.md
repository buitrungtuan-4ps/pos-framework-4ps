# ADR-0171 — CI runs what a change affects

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-13
· Relates to [ADR-0170](0170-repository-layout-code-naming-and-size-rules.md)

The owner approved the standardisation roadmap on 2026-10-13, and with it this record.

## Context

Every pull request runs all thirteen jobs of `.github/workflows/pr.yml`, whatever it touches. A
run takes 9–10 minutes, and a test job has been cancelled at its 10-minute limit.

- **Caching barely helps.** `.github/actions/rust-setup` keys one cache, `target` included, on
  `Cargo.lock` alone. So every job saves under the same key, and whichever job saves first decides
  what all the others restore.
- **The workspace is compiled 5–6 times per run,** with debug info nobody reads.
- **A docs-only pull request** waits as long as a change to `pos-core`.

The baseline is in `docs/metrics/baseline-2026-10.md`.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Raise timeouts and buy larger runners | Pays for waste rather than removing it; still runs everything every time |
| B | A different build system (Bazel, Buck2) | Rewrites the build every contributor knows, for a two-binary repository |
| C | **Fix the cache first, then run only what the change affects, decided from Cargo's own graph** | A few xtask commands and a `changes` job |

## Decision

Option **C**, in two phases.

1. **Phase 1, the quick wins:**
   - a correct cache (`Swatinem/rust-cache` or sccache), saved from `main` only and restored by
     pull requests;
   - no debug info in CI builds;
   - the mold linker on Linux.
2. **Phase 3, run what is affected:**
   - A `changes` job classifies the changed paths (Rust, `ui/`, `dashboard/`, docs, workflows).
   - `cargo xtask affected` turns the changed files into the set of affected crates, using
     `cargo metadata`'s reverse dependencies.
   - Rust tests run under `cargo nextest`, built once into an archive and run in shards.
   - `cargo xtask impact` posts the affected crates, screens and snapshots as a comment on the
     pull request.
3. **What always runs everything:**
   - a change to `pos-proto`, `pos-core`, `Cargo.lock`, `rust-toolchain.toml`, `.github/` or
     `xtask/`;
   - every run on `main`;
   - the nightly run.

   So anything the filter misses is caught within hours, before a release.
4. **Required checks stay valid.** Job names do not change, and a job the filter skips reports
   success, so branch protection needs no edit.
5. **Left to the owner, and not decided here:**
   - enabling a merge queue;
   - paid larger runners.

## Consequences accepted

- **A filter can be wrong.** The full-run triggers above, `main` and nightly bound the cost to a
  few hours of delay, never a release.
- **Each phase is measured** against the baseline before the next starts. The expected figures
  are about 6 minutes after phase 1, and about 3 minutes on average after phase 3, with docs-only
  pull requests near 1 minute.
- **New tools** (`cargo-nextest`, mold, a cache action) are CI-only. Each is pinned by SHA, as
  every action is (`cargo xtask actions-pinned`).
- `release.yml` and branch protection are not touched by this work.
