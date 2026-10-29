---
kind: added
---
- **`cargo xtask hand-types` holds the front ends' hand-mirrored wire types to a falling
  number.** It counts the exported interfaces and types in `dashboard/src/api/types.ts` and
  `ui/src/api/types.ts` against `tools/hand-types-baseline.toml`, failing above it and below it;
  `--write-baseline` lowers a number and refuses to raise one. `just preflight` runs it.
