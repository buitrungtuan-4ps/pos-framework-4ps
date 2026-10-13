---
kind: added
---
- **A pull request gets one comment saying what it affects** (ADR-0171 decision 2).
  - `cargo xtask impact --base <ref>` prints, as Markdown: the path classes, the affected crates,
    the till and console screens whose files changed,
    the snapshots the change touches or the affected crates' tests check, whether
    `docs/generated/` is up to date, and every `pr.yml` job with whether it runs or skips.
  - The new `impact` job posts it on the pull request and edits that comment on each push. It has
    `pull-requests: write`, alone among the jobs; posting never fails the run, and a fork gets the
    step summary instead.
  - A test holds the report's job list to `pr.yml`, so the two cannot drift apart.
