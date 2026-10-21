---
kind: added
---
- **The till's and the console's API types are generated from their OpenAPI documents**
  (ADR-0177 decision 6). `openapi-typescript` 7.13.0 is a pinned development dependency of `ui/` and
  `dashboard/`, so neither app ships it, and `pnpm gen:api` writes `ui/src/api/generated/edge.d.ts`
  from `docs/openapi-edge.json` and `dashboard/src/api/generated/admin.d.ts` from
  `docs/openapi-admin.json`, each headed with the git blob id of its document. `cargo xtask
  api-types --check`, now in `just preflight`, fails when a document changes without its types, and
  needs no Node. Nothing imports the generated types yet: the hand-written ones are replaced module
  by module in later changes. ESLint's size rules skip `src/api/generated/`.
