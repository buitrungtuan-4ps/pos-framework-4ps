---
kind: added
---
- **ADR-0173: the till declares each piece of store data once.** One registry and one engine in
  `ui/` load, retry and refetch everything the till draws. They hold the till behind a boot gate
  with a Retry banner, keep an IndexedDB snapshot of configuration only, and make adding a line,
  changing a quantity and moving a table optimistic. Nothing that moves money is optimistic, and a
  nightly job holds tap-to-paint to a budget. The implementation follows in its own pull requests.
