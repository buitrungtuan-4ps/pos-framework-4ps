---
kind: changed
---
- **A pull request records its changelog entry in a file of its own under `changes/`** (ADR-0170
  decision 5). Every pull request used to edit `[Unreleased]` in `CHANGELOG.md`, so any two open
  pull requests conflicted there. Now each adds `changes/<topic>.md`: front matter naming its
  `kind` and an optional `upgrade_note`, then the entry, as `changes/README.md` describes.
  - `cargo xtask changelog --check` validates the files, and the `docs-gate` job runs it. The job
    accepts either a change file or a `CHANGELOG.md` edit.
  - `cargo xtask changelog` folds the files into `[Unreleased]` and deletes them. It is run by hand
    before a release is tagged.
  - `CONTRIBUTING.md`, `AGENTS.md`, the pull-request template and the release runbook say so.
