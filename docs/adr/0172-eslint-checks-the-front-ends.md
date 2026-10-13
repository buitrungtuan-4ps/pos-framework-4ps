# ADR-0172 — ESLint checks the front ends

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-13
· Relates to [ADR-0170](0170-repository-layout-code-naming-and-size-rules.md)

The owner approved the standardisation roadmap on 2026-10-13, and with it this record:
ADR-0170's Consequences name ESLint as a dependency that needs its own record.

## Context

ADR-0170 decision 3 holds the Rust to size rules, and the rules gate checks them on every pull
request. It sets TypeScript rules too (files of 300–500 lines, functions of up to 50, complexity
up to 10), but nothing measures the two front ends against them. `ui/` and `dashboard/` hold about
53,000 lines of TypeScript between them. `tsc` typechecks them and their own scripts check i18n,
contrast and the step budget, but no linter runs. Their largest files are over 2,000 lines.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Nothing: rely on review | The Rust has a gate and the TypeScript does not, so the TypeScript is where size grows unseen |
| B | Biome | One fast binary for lint and format, but no `complexity` or `max-depth` equal to ESLint's, and its rule set and config still move between releases |
| C | **ESLint with `typescript-eslint`** | Two development dependencies per front end, and a slower lint than Biome |

## Decision

Option **C**.

1. `eslint` and `typescript-eslint` are **development dependencies** of `ui/` and `dashboard/`,
   pinned to exact versions in each `package.json` and lockfile. Nothing ships in a build.
2. Each front end has a flat config (`eslint.config.mjs`) that applies ADR-0170's limits, blank
   lines and comments not counted:

   | Rule | Warn above | Fail above |
   |---|---|---|
   | Lines per file (`max-lines`) | 500 | 1,000 |
   | Lines per function (`max-lines-per-function`) | 50 | 100 |
   | Complexity (`complexity`) | 10 | — |
   | Nesting depth (`max-depth`) | — | 4 |
   | Parameters (`max-params`) | 5 | — |

   No other rule set is switched on here. Adding one, such as `typescript-eslint`'s recommended
   rules, is a separate change.
3. Code that already breaks a fail level is listed in a generated baseline with its current
   figures. A listed file is held to its own figures, so it cannot grow. An entry that is back
   within the limits must be removed, so the list only shrinks, as ADR-0170's Rust ratchet works.
   Per-file `eslint-disable` comments are not allowed.
4. `pnpm lint` runs it, and the `ui` and `dashboard` CI jobs run `pnpm lint`.

## Consequences accepted

- Two more development dependencies in each lockfile, and their updates to review.
- A lint step adds roughly ten to twenty seconds to each front-end job.
- The config and its baseline script are duplicated in both front ends until `packages/shared`
  (ADR-0170 decision 1) gives them one home.
