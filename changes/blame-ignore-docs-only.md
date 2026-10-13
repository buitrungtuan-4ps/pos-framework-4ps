---
kind: fixed
---
- **A pull request that only adds to `.git-blame-ignore-revs` is docs-only** (ADR-0171). The file
  was an unknown path to `cargo xtask affected`, so it ran every CI job; #733 ran all 24 for it. It
  is read by git and GitHub, never by a build or a test, so it now counts as documentation. Every
  other unknown path still runs everything.
