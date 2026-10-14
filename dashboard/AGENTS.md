# `dashboard/` — local rules

Read the root [`AGENTS.md`](../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** the cloud console. A SolidJS and Tailwind app that `pos_cloud` embeds from
`dashboard/dist`, calling the `/admin` API.

## Local rules

- No user-visible string in code. Every string is a key in `src/i18n/en.json` and `vi.json`, and
  both catalogues have the same keys. A setting's title and help are
  `settings.item.<node>.<field>.title` and `.help`.
- Every screen is an entry in `SCREENS` in `src/state/screens.ts`, which feeds the router, the nav,
  the breadcrumbs and the command palette. Never add a route anywhere else.
- The design tokens and the `wcag-contrast`, `i18n-lint`, `i18n-parity` and ESLint-baseline
  scripts live once, in [`packages/shared`](../packages/shared/README.md). Never copy one here
  (`xtask mirrored-files`).
- `src/api/types.ts` is copied by hand from the Rust views, and nothing checks it against Rust.
  Read the handler before changing a type.
- A behaviour change has a vitest test in `tests/*.test.ts(x)`; `pnpm build` runs them.
- `src/installers.mjs` generates the Windows installers under `deploy/edge/`. Change the generator,
  never the generated files.
- Naming: components `PascalCase.tsx`, modules `kebab-case.ts`, hooks `useX`
  ([naming §10.1](../docs/naming-and-api.md#101-code-and-files)).

## Most often touched

`src/i18n/en.json` and `vi.json` · `src/api/types.ts` and `client.ts` · `src/installers.mjs` ·
`src/screens/Settings.tsx` and the other screens

## Gates

The `dashboard` job: `pnpm lint` (ESLint and its baseline), then `pnpm build` (tsc, i18n lint and
parity, contrast, asset weight, installer syntax, step budget, vitest), also with only `dashboard/`
and `packages/shared/` on disk, as the image builds it. The `console-replay` job.
`xtask mirrored-files` · `changes/<topic>.md` for a user-visible change (`docs-gate`, [`changes/README.md`](../changes/README.md)).

## Read first

[`docs/ui-ux.md`](../docs/ui-ux.md) · the [repository map](../docs/repository-map.md) sections on
cloud routes and screens · [ADR-0060](../docs/adr/0060-cloud-back-office-dashboard.md) ·
[ADR-0067](../docs/adr/0067-multi-admin-console-rbac.md).
