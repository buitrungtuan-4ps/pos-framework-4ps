---
kind: added
---
- **Every workspace crate carries a contract card, and CI holds each crate's README to the
  workspace** (ADR-0170 decisions 1 and 5).
  - The card is `[package.metadata.pos]` in each crate's `Cargo.toml`: its layer (`core`, `port`,
    `adapter`, `app`, `testing`, `tool` or `country`), a one-line purpose, the ADRs that govern it,
    and its owners. Cargo ignores it, so no build changes.
  - `cargo xtask crates --check` runs in the `rules` job and in `just preflight`. It requires a
    README and a complete card for every crate, and owners that are the team `CODEOWNERS` routes the
    directory to. Each README's Layer, Owners, Depends on and Used by lines must match the card and
    `cargo metadata`, and a failure prints the line to paste. A change to a crate's dependencies now
    updates both crates' READMEs.
  - The country module READMEs gain the same Layer and dependency lines.
  - `cargo xtask <check>` now works as written in the docs, through an alias in
    `.cargo/config.toml`.
