# Change files

Each pull request records what it changed for a reader of the changelog as **one file here**, rather
than by editing `CHANGELOG.md`, which conflicted between any two open pull requests
([ADR-0170](../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 5).

## One file per pull request

Name it after the branch or the topic, in `kebab-case`: `changes/tax-by-component.md`.

```markdown
---
kind: fixed
upgrade_note: Optional. One paragraph on this one line, when a protocol, migration, permission or default changes.
---
- **A split bill reconciles to the original total.** The rounding remainder goes to the last
  split. (#231)
```

- **`kind`** is one of `added`, `changed`, `deprecated`, `removed`, `fixed` or `security`: the
  heading the entry goes under.
- **`upgrade_note`** is optional, and required in the same cases as before (`CONTRIBUTING.md` §6).
  It is printed as the entry's **Upgrade note** paragraph.
- **The body** is the entry itself, written as before for the person who reads it later: a `- `
  bullet, with continuation lines indented by two spaces.

A pull request with several user-visible changes may add several files.

## What happens to them

- `cargo xtask changelog --check` validates every file here. The `docs-gate` CI job runs it, and
  accepts a new or changed file here as the pull request's changelog entry.
- Before a release is tagged, the person cutting it runs `cargo xtask changelog`. It moves each
  entry to the top of its heading under `[Unreleased]` in `CHANGELOG.md`, in file-name order, and
  deletes the files it folded (`docs/release-runbook.md`).
