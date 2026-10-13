---
kind: changed
---
- **The files the till and the console share live once, in `packages/shared/`** (ADR-0170
  decision 1). They used to be copied into both `ui/` and `dashboard/` and held byte-identical.
  - The six shared files are the design tokens, the contrast gate, the two i18n gates, and
    ESLint's size rules and baseline script.
  - Each front end reaches them as `../packages/shared/…`: a CSS `@import`, a `node` script in
    `package.json`, and a small `eslint.config.mjs`.
  - The cloud image's dashboard stage copies `packages/shared/` beside `dashboard/`, and the
    `dashboard` CI job builds from just those two.
  - `xtask mirrored-files` now fails if a shared file is missing or a copy reappears.
  - `cargo xtask code-stats --check --base` holds each front end's `eslint-baseline.json` to the
    same rule as the Rust size baseline: it may only shrink, unless the pull request says
    `size-baseline: grow <reason>`.
