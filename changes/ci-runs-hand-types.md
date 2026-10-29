---
kind: changed
---
- **CI runs `cargo xtask hand-types`.** The `rules` job now fails a pull request that adds a
  hand-mirrored wire type to a front end's `src/api/types.ts`; the type comes from `schemas.ts`
  instead. `tools/hand-types-baseline.toml` gains an owner in `CODEOWNERS`.
