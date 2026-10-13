---
kind: added
---
- **`pnpm lint` holds the till and the console to ADR-0170's TypeScript size limits** (ADR-0172).
  - ESLint 10.11.0 and typescript-eslint 8.70.1 are exact-pinned development dependencies of `ui/`
    and `dashboard/`.
  - Code fails above these limits: a file over 1,000 lines, a function over 100 (neither counting
    blank lines and comments), or nesting deeper than 4. It is warned about above 500 lines,
    50-line functions, complexity 10 or 5 parameters.
  - Files already over a fail level are listed in each root's `eslint-baseline.json` and may not
    grow: 13 in `ui/` and 56 in `dashboard/`. An entry that is no longer needed fails `pnpm lint`
    until it is removed.
  - `eslint-disable` comments have no effect.
  - The `ui` and `dashboard` CI jobs run `pnpm lint`, and `xtask mirrored-files` keeps the config
    and its script identical in both roots.
