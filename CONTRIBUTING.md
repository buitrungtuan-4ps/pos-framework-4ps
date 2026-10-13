# Contributing

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

This guide gets a new contributor — human or AI — from zero to a reviewable pull request. Read [`AGENTS.md`](AGENTS.md) first; it contains the rules. This file explains the workflow.

## 1. Orientation in five minutes

The mental model is four layers, and every file belongs to exactly one:

```
pos-core     the rules        pure logic, no I/O, tests run in milliseconds
pos-ports    the boundary     traits we own; the only way the core sees the world
adapters/    the world        one crate per external system (SQLite, NATS, printer, vendor, country)
pos-edge     the wiring       binaries that plug adapters into the core
pos-cloud
```

To answer "where does my change go?", use the table in [`AGENTS.md`](AGENTS.md) §5. To answer "why is it like this?", read [`docs/adr/`](docs/adr/). To answer "what should it do?", read [`docs/pos-spec.md`](docs/pos-spec.md) — every rule there has a section number you can cite in a PR.

## 2. Set up and verify

```bash
just preflight           # format, lint, unit tests, licences, naming, snapshots — must be green
just test-integration    # needs Docker: PostgreSQL + NATS
just simulate            # synthetic fleet: sync, offline bursts, OTA rings
just run-edge            # a store on your machine, UI at http://localhost:8080
just run-cloud           # the control plane on your machine
```

**CI runs the tests with [cargo-nextest](https://nexte.st/)**, each test in its own process, built once and split into three shards. `just test` keeps `cargo test`, and the two run the same tests. To run them the way CI does, install the version pinned in `tools/tool-versions.toml` (`[cargo-nextest]`), then:

```bash
cargo nextest run --workspace --profile ci   # what the three shards run between them
cargo test --workspace --doc                 # the doc tests, which nextest does not run
```

The `ci` profile in `.config/nextest.toml` reports every failure rather than stopping at the first, and never retries. A test that passes under `cargo test` but not under nextest usually shares state with another test through a global, an environment variable or a port. Fix the test, or put it in a `test-groups` entry in `.config/nextest.toml`. Never skip it.

If `just preflight` fails, fix that before anything else. Do not invent alternative build commands; if one is missing, add it to the `justfile` in its own PR.

## 3. Before writing code

- An issue exists and describes the problem, not the solution.
- If the change touches a **port**, `pos-proto`, a dependency, or a security or data-retention boundary: an ADR must be merged **first** ([`docs/adr/`](docs/adr/)).
- Find the rule you are implementing in `docs/pos-spec.md` or `docs/architecture.md` and cite its section in the PR. If the rule does not exist yet, propose it in the same PR — code and specification land together.

## 4. Making the change

- Branch from `main`: `feat/short-description`, `fix/short-description`. Keep it alive less than three days.
- Commit with [Conventional Commits](https://www.conventionalcommits.org/) and a crate scope: `feat(fiscal-vn): issue invoice from allocated range`.
- Keep pull requests small and single-purpose — around 400 changed lines is the guideline, not a hard limit.
- Fill in the pull-request template completely. Empty checkboxes are treated as unfinished work.
- Label AI-assisted work `ai-assisted`. It does not lower the bar; it makes provenance traceable.
- Squash merge. A human presses the button.

**Size rules** ([ADR-0170](docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 3) are checked by `cargo xtask code-stats --check`, in `just preflight` and the `rules` job:

| | Warn above | Fail above |
|---|---|---|
| A Rust file outside `tests/` | 800 lines | 1,000 lines |
| A Rust file under `tests/` or `benches/` | 1,200 lines | 1,500 lines |
| A function outside test code | 60 lines | 100 lines |
| Its parameters | 7 | 8 |

- Code that broke the rules when the ratchet began is listed in [`tools/code-size-baseline.toml`](tools/code-size-baseline.toml) with its figure. A listed item may grow by 10 lines at most, and by no parameters. Anything larger goes in a new module or function. A file or function is never split just to meet a number; split it by meaning.
- When a listed item is back within the rules, or is gone, `--check` fails until its entry is removed. Remove it by hand, or run `cargo xtask code-stats --write-baseline` and check the diff only removes or lowers entries.
- **The baseline may only shrink.** With `--base`, `--check` compares the file with its copy at the merge base, and a new entry or a raised figure fails. A pull request that must grow it says why in its body, on a line of its own: `size-baseline: grow <reason>`. On `main`, the comparison is with the parent commit and only warns. `CODEOWNERS` routes the file to the architecture maintainers.
- A function is keyed by its file and qualified name (`crates/pos-edge/src/app.rs::Edge::open_bill`), so moving it within its file changes nothing. Moving it to another file, or renaming it, moves its entry.
- Warn levels are reported as annotations on the files a pull request changes. They never fail it.

## 5. Documentation is part of the change

**A behaviour change updates its documentation in the same pull request.** Reviewers verify this; it is a checkbox they are expected to reject on.

| You changed | Update |
|---|---|
| Business behaviour, a rule, a state transition | `docs/pos-spec.md` |
| A component, port, or infrastructure decision | `docs/architecture.md` (+ an ADR if it is a decision) |
| A field, endpoint, event, or permission name | `docs/naming-and-api.md` and the matching snapshot |
| A screen, interaction, or state | `docs/ui-ux.md` |
| Workflow, CI, release process | `docs/engineering-guide.md` |
| Capacity, limits, failure behaviour | `docs/capacity-and-reliability.md` |
| A term a newcomer would not know | `docs/glossary.md` |
| A crate's dependencies, or a new crate | its `README.md` and its `[package.metadata.pos]` card, and the `README.md` of each crate on the other end of a changed dependency |

**Every workspace crate has a `README.md` and a contract card**, `[package.metadata.pos]` in its `Cargo.toml`: its layer, a one-line purpose, its ADRs and its owners ([ADR-0170](docs/adr/0170-repository-layout-code-naming-and-size-rules.md)). `cargo xtask crates --check`, which `just preflight` and CI run, holds them to the workspace. The README's Layer and Owners line must match the card. Its Depends on and Used by lines must list exactly what `cargo metadata` reports. The owners must be the team `CODEOWNERS` routes the directory to. Each failure prints the line to paste.

`docs/generated/` holds the crate graph and the ADR index, which `cargo xtask gen-docs` writes from the tree. The graph changes when a crate's dependencies or card do. The index lists only the ADRs nothing cites and any citation of an ADR that does not exist (`ADR-NNNN`, or a link to `adr/NNNN-…`, in the code or `.github/`), so it changes rarely. `cargo xtask gen-docs --check` fails until both are regenerated, and fails on any citation of a missing ADR. Which files cite an ADR is not committed, because it changes in almost every pull request: `cargo xtask gen-docs --adr-citations [ADR-NNNN]` prints it.

Generated documents — OpenAPI, the permission matrix, the event catalogue — are **never** hand-edited. Regenerate them. `.gitattributes` marks every generated file `linguist-generated`, with the command that rewrites it, so a pull request's diff collapses them.

## 6. Changelog and release notes

Every user-visible change adds **a change file** under [`changes/`](changes/README.md), one per pull request, rather than editing `CHANGELOG.md`, whose `[Unreleased]` section conflicted between any two open pull requests ([ADR-0170](docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 5). Its front matter names the category, and its body is the entry, written for the person who will read it later:

```markdown
---
kind: fixed
---
- Split bills now reconcile to the original total when the amount does not divide evenly;
  the rounding remainder is assigned to the final split. (#231)
```

Add an `upgrade_note:` to the front matter whenever a change affects `PROTOCOL_VERSION`, a database migration, a permission identifier, or a default value. Release notes are assembled from the changelog plus those upgrade notes, so an omission here becomes an omission in the release. `cargo xtask changelog --check` validates the files, as CI does, and `cargo xtask changelog` folds them into `CHANGELOG.md` `[Unreleased]` before a release is tagged.

Every fix references its issue number. Every release states its SemVer, its `PROTOCOL_VERSION`, its MSRV, and — in one plain sentence — what changes for restaurant staff.

## 7. What reviewers check

The ten-point list at the end of [`docs/design-principles.md`](docs/design-principles.md), plus: does it belong in this layer, are the tests meaningful rather than coverage padding, and would someone reading this in a year understand why. Disagreements that outlast a day become an ADR rather than a longer comment thread.

## 8. Security

Never commit secrets, keys, or `.env` files — CI scans for them, but the scan is a safety net, not a permission slip. Never modify signing keys, release workflows, or branch protection. Report suspected vulnerabilities privately per `SECURITY.md`; do not open a public issue.
