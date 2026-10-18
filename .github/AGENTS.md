# `.github/` — local rules

Read the root [`AGENTS.md`](../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** the CI workflows (`workflows/`), their shared setup (`actions/rust-setup`,
`actions/node-setup`), the issue and pull-request templates, and Dependabot.

## Local rules

- Every change here needs an owner review (`@maintainers-architecture`).
- An agent never edits `workflows/release.yml`, branch protection, or anything holding a signing
  key (root `AGENTS.md` §8).
- Every `uses:` is pinned to a full commit SHA, with the version in a comment
  (`xtask actions-pinned`).
- A cache is saved only from `main`, and its key carries the build environment, so `pr.yml` and
  `main.yml` keep the same `env` ([ADR-0171](../docs/adr/0171-ci-runs-what-a-change-affects.md)).
- Each job has a `timeout-minutes`. A new gate goes in the `rules` job when it is an xtask check,
  ordered so the cheapest failure surfaces first.
- A secret is read only where a step needs it, never logged, and listed in
  [`docs/fork-checklist.md`](../docs/fork-checklist.md).

## Most often touched

`workflows/pr.yml` · `workflows/main.yml` · `workflows/nightly.yml` · `workflows/station.yml` ·
`workflows/deploy.yml`

## Gates

`xtask actions-pinned` · owner review · the workflows themselves on the pull request.

## Read first

[`docs/engineering-guide.md`](../docs/engineering-guide.md) §4 and §6 ·
[ADR-0171](../docs/adr/0171-ci-runs-what-a-change-affects.md) ·
[`docs/release-runbook.md`](../docs/release-runbook.md).
