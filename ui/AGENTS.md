# `ui/` — local rules

Read the root [`AGENTS.md`](../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** the till. A SolidJS and Tailwind app that `pos_edge` embeds from `ui/dist`
and serves to every device in the store. See [`README.md`](README.md).

## Local rules

- No user-visible string in code. Every string is a key in `src/i18n/en.json` and `vi.json`, and
  both catalogues have the same keys.
- A screen adapts to a device class, never a pixel width ([`docs/ui-ux.md`](../docs/ui-ux.md) §1,
  `scripts/device-classes.mjs`). The accent is
  never on a button (`scripts/token-roles.mjs`). Colours come from the shared tokens.
- The design tokens and the `wcag-contrast`, `i18n-lint`, `i18n-parity` and ESLint-baseline
  scripts live once, in [`packages/shared`](../packages/shared/README.md). Never copy one here
  (`xtask mirrored-files`).
- `src/api/types.ts` is copied by hand from the Rust views, and nothing checks it against Rust.
  Read the handler before changing a type.
- A new flow, or a new route a screen calls, gets a flow in `tests/replay.spec.mjs`. Wait for the
  real precondition, never a sleep.
- Naming: components `PascalCase.tsx`, modules `kebab-case.ts`, hooks `useX`
  ([naming §10.1](../docs/naming-and-api.md#101-code-and-files)).

## Most often touched

`tests/replay.spec.mjs` · `src/i18n/en.json` and `vi.json` · `src/api/types.ts` and `client.ts` ·
`src/state/store.ts` · `src/screens/<Screen>.tsx`

## Gates

The `ui` job: `pnpm lint` (ESLint and its baseline), then `pnpm build` (tsc, i18n lint and parity,
contrast, token roles, device classes, step budget). The `replay` job: Playwright against
`examples/minimal-edge`. `xtask mirrored-files` · `changes/<topic>.md` for a user-visible change (`docs-gate`, [`changes/README.md`](../changes/README.md)).

## Read first

[`docs/ui-ux.md`](../docs/ui-ux.md) · the [repository map](../docs/repository-map.md) sections on
edge routes and screens · [ADR-0109](../docs/adr/0109-counting-the-taps-an-operator-makes.md).
