# `packages/shared` — what both front ends share

**Owner** @maintainers-architecture · [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 1

The till (`ui/`) and the console (`dashboard/`) are separate pnpm packages with separate Vite
builds. What they must agree on lives here, once, and each reaches it as `../packages/shared/…`:

| File | What it is | How a front end uses it |
|---|---|---|
| `styles/tokens.css` | The design tokens: spacing, type, touch, radius, motion and the three colour palettes | `@import` in `src/app.css` |
| `scripts/wcag-contrast.mjs` | The WCAG-AA contrast gate over those tokens ([audit](../../docs/wcag-contrast-audit.md)) | `pnpm contrast`, in `pnpm build` |
| `scripts/i18n-lint.mjs` | No hardcoded user-visible strings ([ADR-0020](../../docs/adr/0020-i18n-runtime.md)) | `pnpm i18n:lint`, in `pnpm build` |
| `scripts/i18n-parity.mjs` | Every locale has the same keys as English | `pnpm i18n:parity`, in `pnpm build` |
| `eslint/size.mjs` | ESLint's size rules ([ADR-0172](../../docs/adr/0172-eslint-checks-the-front-ends.md)) | imported by `eslint.config.mjs` |
| `scripts/eslint-baseline.mjs` | Writes and checks a front end's `eslint-baseline.json` | `pnpm lint` |

## Rules

- **No copies.** `cargo xtask mirrored-files` fails if one of these files is missing here, or if a
  copy appears in `ui/` or `dashboard/`.
- **Nothing here imports a package.** This directory has no `node_modules`. A script runs from the
  front end's root and resolves `typescript` or `eslint` from there; `eslint/size.mjs` is handed
  ESLint's pieces by the `eslint.config.mjs` that calls it.
- **The cloud image copies this directory** beside `dashboard/` (`deploy/Dockerfile`). The
  `dashboard` CI job builds from just those two, so a file here that reads anything else fails in
  CI, not in the image.

## How to test

From `ui/` or `dashboard/`: `pnpm build` runs the gates, and `pnpm lint` runs ESLint.
