---
kind: added
---
- **A package version carries its flows and reactions.** `POST /admin/packages/{package_id}/versions`
  and its dry run take `flows` and `reactions` beside the screens and data, and check them as the
  store does when it applies them: ids, steps, prerequisites without a cycle, a reaction that moves
  no money, and every condition compiled in the rules dialect, an edge guard reading only what the
  edge holds. A dry run's refusal names the flow and the step, or the reaction, it is about.
