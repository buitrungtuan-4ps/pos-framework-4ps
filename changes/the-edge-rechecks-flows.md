---
kind: added
---
- **The edge enforces a package's flow guards.** A package's flows are checked when the edge
  applies it (limits, ids, references, cycles, and every condition compiled, a guard reading only
  what the edge holds), and a failing flow refuses the whole package. Before adding a line, firing,
  and opening a bill, the edge answers each guard from the order's own facts (its guest count, its
  line count and its channel), and refuses an act whose step is not done with
  `409 FLOW_PREREQUISITE_UNMET`, naming the step; the till says which step comes first (ADR-0192).
  A store with no package is untouched.
