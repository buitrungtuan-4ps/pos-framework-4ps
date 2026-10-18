# Plans

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-13

A **plan** is a working document for work that spans many sessions and pull requests. It says where
the work stands today and what comes next, and it is edited as the work moves.

| | A plan | An ADR | A spec (`docs/*.md`) |
|---|---|---|---|
| Says | what we are doing, and how far we are | why a decision was made | how the system behaves now |
| Changes | every time a step moves | never (superseded instead) | with the behaviour it describes |
| Ends | when the work is done; then it is kept as the record | when superseded | never |

A plan never decides anything on its own: a decision it needs goes in an ADR, and the plan links it.
When a plan is finished, its status says so and it stays here.

## Template

```markdown
# <Plan name>

**Status** Active | Paused | Done · **Owner** @team · **Last updated** YYYY-MM-DD

## Goal

What is true when this is finished, in two or three sentences.

## Steps

| Step | What | Status | Pull requests |
|---|---|---|---|
| 1 | … | Done / In progress / Next / Not started / Blocked: <on what> | #123 |

## Decisions taken

- <date> — <decision>, and the ADR or pull request that records it.

## Open questions

- <question> — who answers it.

## Links

The ADRs, issues and documents the plan depends on.
```

## Plans

| Plan | Status |
|---|---|
| [standardisation.md](standardisation.md) | Active: phase 2 |
| [till-store-data.md](till-store-data.md) | Done |
