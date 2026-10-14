---
kind: changed
---
- **The docs gate no longer asks a Dependabot bump for a change file.** A pull request opened by
  `dependabot[bot]` whose every changed file is a `Cargo.toml`, `Cargo.lock`, `package.json` or
  `pnpm-lock.yaml` passes `docs-gate` without one; `cargo xtask changelog --exempt` makes the call,
  with the author passed in from the event rather than written into the script. Every other pull
  request, including a Dependabot branch someone has pushed a source change onto, keeps the gate.
