# Standardisation

**Status** Active · **Owner** @maintainers-architecture · **Last updated** 2026-10-13

## Goal

The repository is organised so that a person or an AI agent can find the right file, change it
without colliding with other work, and have CI check the rules: written layout, naming and size
rules ([ADR-0170](../adr/0170-repository-layout-code-naming-and-size-rules.md)), CI that runs what a
change affects ([ADR-0171](../adr/0171-ci-runs-what-a-change-affects.md)), a modular monolith of
small crates, and one source of truth per fact. POS behaviour does not change, nothing published
is renamed, and the system stays two binaries. The owner approved the roadmap on 2026-10-13.

## Steps

| Phase | What | Status | Pull requests |
|---|---|---|---|
| 0. Prepare and measure | CI test timeout 15 min; ADR-0170 and ADR-0171; the code-stats baseline; changesets under `changes/`; generated files marked | Done | #700, #701, #702, #703, #704 |
| 1. CI quick wins | Per-job caches saved from `main`, no debug info, mold | Done | #705 |
| 2. Rules, map, AI context | Naming rules, repository map, docs index (2.1); crate READMEs and contract cards (2.2); crate graph and ADR index (2.3); short root and nested `AGENTS.md`, docs drift (2.4); plans and root cleanup (2.5); size ratchet gate (2.6); ESLint (2.7); `packages/shared` (2.8) | In progress: 2.1–2.3 merged; 2.4 drift fixes in review, its `AGENTS.md` drafts with the owner; 2.5 in review | #707, #708, #709, #710 |
| 3. CI runs what changed | A `changes` job with path classes, `xtask affected`, nextest, build once and shard, `xtask impact` on each PR; `main` and nightly run everything | Not started; needs 1 and 2 | |
| 4. Pilot | Reporting split into modules, then `pos-cloud-reports` on a `pos-cloud-api` base; the five longest functions split; build, token and review time measured | Not started; needs 2 and 3. **Decision gate 1** follows | |
| 5. Module split | `http.rs`, `app.rs`, `persistence.rs`, `printing.rs` and the large test files into modules of ≤ 500 lines by business area and role; a route declared in one place | Not started; needs 4, and the open Jules and Dependabot pull requests cleared first | |
| 6. Crate split | 14 crates (9 cloud, 5 edge) under `crates/core`, `crates/edge`, `crates/cloud` | Not started; needs 5. **Decision gate 2** after the first 2–3 crates | |
| 7. Long functions | The 55 functions over 100 lines down to ≤ 60, 3–5 per PR with tests and a mutation check | Not started; needs 5; may run beside 6 | |
| 8. Generate and self-register | TypeScript types from 100% OpenAPI; one screen registry; map, graph and CODEOWNERS from the contract cards; `cargo xtask new crate\|route\|setting\|screen\|adapter\|country` | Not started; needs 6 | |
| 9. Framework guides | Add a country, write an adapter, add a setting, add a business area, each with a runnable example; optionally a merge queue and larger runners | Not started; needs 8 | |

## Decisions taken

- 2026-10-13 — The owner approved the roadmap, ADR-0170 and ADR-0171, and lets the work
  self-approve what best practice settles, collecting the rest for the owner.
- 2026-10-13 — A pull request records its changelog entry as a file under `changes/` (ADR-0170
  decision 5, #703).
- 2026-10-13 — Each crate's owners in its contract card are the CODEOWNERS team that routes its
  directory, else `@maintainers-architecture` or `@maintainers-cloud` (#709).
- 2026-10-13 — Which files cite each ADR is printed on demand
  (`cargo xtask gen-docs --adr-citations`), not committed, because it changes with almost every
  pull request; only uncited ADRs and citations of missing ones are committed (#710).
- 2026-10-13 — Changes to `AGENTS.md`, and new nested `AGENTS.md` files, are applied by the owner,
  not by an agent (ADR-0126's reasoning). The phase 2.4 drafts wait for the owner.

## Open questions

- The phase 2.4 `AGENTS.md` rewrite and nested files: owner to review and apply.
- Decision gate 1, after phase 4: continue to the module and crate split, or stop with phases 0–3
  kept. Owner.
- Decision gate 2, after the first 2–3 crates of phase 6. Owner.
- A merge queue and larger runners (ADR-0171, not decided there). Owner.
- Root directories outside ADR-0170's layout. None moves in phase 2, since each has references:

  | Directory | Proposal | When |
  |---|---|---|
  | `k8s/` | `deploy/k8s/`, beside the Compose stack it is the alternative to. Two Markdown links and a few comments move with it; ADRs that name the old path stay as written | Its own small pull request |
  | `templates/adapter-template/` | Replaced by `cargo xtask new adapter`, then removed | Phase 8 |
  | `examples/minimal-edge/` | `crates/testing/`, beside the fakes it boots on | Phase 6 |
  | `apps/pos-station/` | Stays: a Tauri app with its own lockfile, outside the workspace ([ADR-0147](../adr/0147-pos-station-is-a-tauri-shell-over-the-edge.md)). The layout should name `apps/` | An ADR-0170 follow-up |
  | `tools/` | Stays: the allow-lists and baselines the xtask checks read | — |
  | `.jules/` | Stays: the notebook the Jules agent reads and writes at that path. Moving it would break the tool | — |

## Links

[ADR-0170](../adr/0170-repository-layout-code-naming-and-size-rules.md) ·
[ADR-0171](../adr/0171-ci-runs-what-a-change-affects.md) ·
[baseline figures](../metrics/baseline-2026-10.md) ·
[repository map](../repository-map.md) · [docs index](../README.md)
