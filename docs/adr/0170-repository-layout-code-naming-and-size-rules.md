# ADR-0170 — Repository layout, code naming and size rules

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-13
· Relates to [ADR-0006](0006-ports-and-adapters.md), [ADR-0010](0010-naming-standard.md),
[ADR-0171](0171-ci-runs-what-a-change-affects.md)

The owner approved the standardisation roadmap on 2026-10-13, and with it this record.
POS behaviour does not change, and the system stays two binaries.

## Context

The core is sound: a clean domain behind 19 ports, contract tests, and configuration instead of
code. What does not scale is how the code is organised. Measured on `main` on 2026-10-08:

- `pos-cloud/src/http.rs` is 38,555 lines. 37 of 497 Rust files exceed 1,000 lines.
- 653 of 8,138 functions exceed 40 lines, and 55 exceed 100.
- 26% of last month's commits touched `pos-edge/src/app.rs`, so two changes rarely run in parallel.
- About 3,150 lines of TypeScript types are copied by hand from Rust, and a console route is added
  in 6–7 places.
- `CHANGELOG.md` is 1.24 MB and conflicts in every pair of open pull requests.
- 4 of 29 crates have a README.

The exact figures are recorded in `docs/metrics/baseline-2026-10.md`.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Leave the layout, and enforce review discipline | Nothing stops a file growing; people and AI agents keep reading 38k-line files |
| B | Split into services | Network boundaries, deployment and operations cost the stores cannot carry; rejected by [ADR-0006](0006-ports-and-adapters.md)'s two-binary design |
| C | **A modular monolith: modules, then crates, under written rules that CI checks** | One-off moving work, gated by two decision points |

## Decision

Option **C**.

1. **Target layout.**
   - **Crates:** `crates/core` (`pos-proto`, `pos-core`, `pos-ports`), `crates/edge`,
     `crates/cloud`, `crates/adapters` and `crates/testing` (fakes, contract tests, simulator).
   - **Other code:** `countries/<iso2>` and `packages/shared` (UI tokens and the scripts `ui/` and
     `dashboard/` share). `ui/` and `dashboard/` keep their names.
   - **Docs:** a Diátaxis index (`docs/README.md`), `docs/repository-map.md` ("to do X, edit
     these files"), `docs/generated/` (dependency graph, ADR-to-code index), `docs/plans/` for
     multi-session work, and `docs/archive/vi/`.
   - **Change entries:** `changes/` (decision 5).
   - **Directory notes:** every directory has a `README.md`, and every source file opens with what
     it does, its ADRs and its related files.
2. **Naming.**
   - API, events and data keep Google AIP (`docs/naming-and-api.md`). Nothing published is
     renamed.
   - Rust follows the Rust API Guidelines.
   - TypeScript names components `PascalCase.tsx` and modules `kebab-case.ts`.
   - Countries use ISO 3166-1, currencies ISO 4217, languages BCP 47, and times RFC 3339 UTC.
   - Code is in English. Vietnamese appears only in i18n catalogues and `docs/archive/vi`.
3. **Size rules.** New code must meet them. Existing code is listed in a baseline file that may
   only shrink (a ratchet).

   | Rule | Target | Warn | Block new code |
   |---|---|---|---|
   | Lines per source file | ≤ 500 | > 800 | > 1,000 |
   | Lines per test file | ≤ 800 | > 1,200 | > 1,500 |
   | Lines per function | ≤ 40 | > 60 | > 100 |
   | Parameters per function | ≤ 5 | > 7 | > 8 |
   | Nesting depth | ≤ 3 | > 4 | > 5 |

   - Line width stays 100 (rustfmt).
   - TypeScript is checked by ESLint, with files ≤ 300–500 lines, functions ≤ 50 lines and
     complexity ≤ 10.
   - `cargo xtask code-stats` measures all of it.
4. **A modular monolith, not services.**
   - First, files split into modules by business area and role (handlers, views, validation,
     persistence).
   - Then into 14 crates: `pos-cloud-catalog`, `-fees`, `-config`, `-people`, `-devices`, `-floor`,
     `-inventory`, `-admin`, `-integrations`, and `pos-edge-app`, `-billing`, `-shift`, `-printing`,
     `-floor`. They sit on the pilot's `pos-cloud-api` and `pos-cloud-reports`.
   - `pos-edge` and `pos-cloud` remain the two binaries, and only compose these crates.
   - Two decision gates let the owner stop with everything done so far kept: one after the
     reporting pilot, with build, token and review numbers; one after the first three crates.
5. **One source of truth per fact.**
   - A route is declared once; that one declaration gives the router, the permission, OpenAPI and
     the snapshot.
   - TypeScript types are generated from OpenAPI once its coverage is 100%. Until then they stay
     hand-written.
   - Screens are listed in one registry.
   - Each crate carries a contract card, `[package.metadata.pos]`, from which the map, the
     dependency graph and CODEOWNERS are generated.
   - `cargo xtask new crate|route|setting|screen|adapter|country` scaffolds the full set of files.
   - **Changesets replace hand-editing `CHANGELOG.md`.** Each pull request adds one file under
     `changes/`, and `cargo xtask changelog` folds them in before a release.
6. **Move-only pull requests** may exceed the 400-line guideline when `git diff --color-moved`
   shows they only move code. Their commits are listed in `.git-blame-ignore-revs`.
7. **A long function is split only where tests cover it,** with a mutation check on each split and
   3–5 functions per pull request.

## Consequences accepted

- Roughly 210–290 hours of work, with feature work paused meanwhile. Urgent fixes still ship.
- **More files:** about 500 Rust files become 900–1,100. The repository map and contract cards are
  what keep that navigable. Files are split by meaning, never to hit a line count.
- **Moves collide with open pull requests,** so open pull requests are cleared before the module
  split, and each file is frozen while it moves.
- **History:** `git blame` crosses moves only through `.git-blame-ignore-revs` and `git log --follow`.
- **New dependencies** (codegen, ESLint, CI tools) each still need their own ADR (AGENTS.md §7).
- Nothing on the wire, in events or in snapshots changes. `PROTOCOL_VERSION` stays as it is.
