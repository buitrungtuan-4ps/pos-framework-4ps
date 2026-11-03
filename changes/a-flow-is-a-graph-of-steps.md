---
kind: added
---
- **A package can carry flows, and the core checks and runs them.** `pos-proto` gains
  `pos_proto::flow` (a flow's steps: screen or action, required or optional, `applies_when`,
  prerequisites and the edge's `done_when`) and an additive `flows` field on a package version's
  content. `pos_core::flow` refuses a malformed flow at publish and on apply (limits, ids,
  references, cycles, conditions), says which steps the till may offer and which prerequisite is
  missing, and finds the guard an edge command would break (ADR-0192). Nothing publishes or
  enforces a flow yet, so no store behaves differently.
