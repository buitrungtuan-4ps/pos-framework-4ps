---
kind: added
---
- **A guide to adding a business area.** `docs/guides/add-a-business-area.md` says which ADRs a new
  area needs before any code, that it goes into an existing area crate or `pos-cloud` rather than a
  new crate (ADR-0175), and where each part goes, then follows reason codes through every tier,
  quoting the code as it is. It ends with the crate and screen generators run on illustrative names,
  and the checks that prove what they wrote. `just guides-check` holds every quote and command to
  the code, and `just guides` runs the steps.
