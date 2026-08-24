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

Generated documents — OpenAPI, the permission matrix, the event catalogue — are **never** hand-edited. Regenerate them.

## 6. Changelog and release notes

Every user-visible change adds an entry to [`CHANGELOG.md`](CHANGELOG.md) under `[Unreleased]`, in the correct category, written for the person who will read it later:

```markdown
### Fixed
- Split bills now reconcile to the original total when the amount does not divide evenly;
  the rounding remainder is assigned to the final split. (#231)
```

Add an **Upgrade note** in the same entry whenever a change affects `PROTOCOL_VERSION`, a database migration, a permission identifier, or a default value. Release notes are assembled from the changelog plus those upgrade notes, so an omission here becomes an omission in the release.

Every fix references its issue number. Every release states its SemVer, its `PROTOCOL_VERSION`, its MSRV, and — in one plain sentence — what changes for restaurant staff.

## 7. What reviewers check

The ten-point list at the end of [`docs/design-principles.md`](docs/design-principles.md), plus: does it belong in this layer, are the tests meaningful rather than coverage padding, and would someone reading this in a year understand why. Disagreements that outlast a day become an ADR rather than a longer comment thread.

## 8. Security

Never commit secrets, keys, or `.env` files — CI scans for them, but the scan is a safety net, not a permission slip. Never modify signing keys, release workflows, or branch protection. Report suspected vulnerabilities privately per `SECURITY.md`; do not open a public issue.
