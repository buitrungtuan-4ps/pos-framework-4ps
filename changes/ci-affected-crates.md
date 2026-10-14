---
kind: changed
---
- **CI tests, lints and builds only the crates a change can affect** (ADR-0171 decision 2).
  - `cargo xtask affected --crates` lists the crates holding a changed file and every crate that
    depends on them, tests included. `test`, `clippy`, `build` and the doc tests run on that list,
    and on the whole workspace when a core crate, the lockfile or another run-everything path
    changed.
  - A change to a file a crate compiles in or tests against now runs that crate's tests, even
    though the file lives outside it: a snapshot, a generated document, the console's i18n
    catalogue, or the edge installer. Before, such a change could skip them.
