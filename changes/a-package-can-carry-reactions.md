---
kind: added
---
- **A package can carry reactions, and the store server checks them.** `pos-proto` gains
  `pos_proto::reaction` (*when* a trigger fires on the till, *if* a condition holds, *then* one
  catalogue act with fixed parameters) and an additive `reactions` field on a package version's
  content. `pos_core::reaction::check_reactions` refuses a reaction whose trigger is unknown or
  names the wrong screen or step, whose trigger has more than 16 reactions, whose act moves money
  or calls out, whose parameters are not the act's, or whose condition does not compile; the store
  server applies a package only if its reactions pass (ADR-0193 decision 5). Nothing runs a
  reaction yet, so no store behaves differently.
