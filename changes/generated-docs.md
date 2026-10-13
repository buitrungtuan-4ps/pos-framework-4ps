---
kind: added
---
- **`docs/generated/` holds a crate graph and an ADR index, generated from the tree** (ADR-0170
  decision 1).
  - `crate-graph.md` shows the workspace crates by layer as a Mermaid graph, with a table of each
    crate's purpose and dependents. It comes from `cargo metadata` and the contract cards.
  - `adr-index.md` lists the ADRs nothing in the code cites, and any citation of an ADR that does
    not exist.
  - `cargo xtask gen-docs` writes both. `cargo xtask gen-docs --check` runs in the `rules` job and in
    `just preflight`. It fails until both files are regenerated, and it fails on any citation of an
    ADR that does not exist.
  - `cargo xtask gen-docs --adr-citations [ADR-NNNN]` prints which files cite each ADR. That map
    changes with almost every pull request, so it is not committed.
