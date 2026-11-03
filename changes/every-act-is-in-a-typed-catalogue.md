---
kind: added
---
- **Every act a package may name is in a typed catalogue.** `pos-proto` gains `actions`: 45 acts
  (`sales.line.add`, `billing.bill.settle`, `ui.screen.open` …), each with its effect class
  (`ACTION_EFFECT_UI`, `_EDGE_WRITE`, `_MONEY`; `_EXTERNAL` waits for the cloud gateway), its tap
  frequency, typed parameters, the edge routes it calls, the permissions they need, the refusals the
  till can predict and the release that introduced it (ADR-0191). A test holds each act's
  permissions to the route table and its effect class to the edge's money commands, and
  `docs/snapshots/actions.txt` refuses a removed act, effect or parameter. Nothing reads the
  catalogue yet, so no till or store behaves differently.
