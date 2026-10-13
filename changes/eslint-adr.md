---
kind: added
---
- **ADR-0172: ESLint checks the front ends.** `eslint` and `typescript-eslint` become pinned
  development dependencies of `ui/` and `dashboard/`, applying ADR-0170's TypeScript size limits.
  Existing code over a fail level goes in a baseline that only shrinks, and `pnpm lint` runs in
  both front-end CI jobs. The setup follows in its own pull request, once this record is merged.
