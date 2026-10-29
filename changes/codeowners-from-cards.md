---
kind: added
---
- **The contract cards say who owns each crate, and CI holds `CODEOWNERS` to them** (ADR-0170
  decision 5).
  - `cargo xtask gen-docs` writes `docs/generated/codeowners-crates.txt`: the line `CODEOWNERS`
    would hold for each workspace crate, from its card's `owners`.
  - `gen-docs --check` fails where a crate's own line in the root `CODEOWNERS` names other owners
    than its card, and prints the line the card asks for.
  - Nothing writes `CODEOWNERS`. A crate with no line there is not a failure; adding one is a change
    a person makes.
