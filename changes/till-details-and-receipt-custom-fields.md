---
kind: added
---
- **The till types a bill's custom fields, and a receipt template chooses which custom fields print**
  (phase 10 open items O7 and O8). A package's fields on bills now have a **Details** panel on the
  pay screen, which reads the bill's values and saves the changed ones (a personal value only with
  its consent ticked, never shown again). The console's receipt template editor offers each order or
  bill field the tenant's packages declare as a switch, and writes the template's
  `custom_field_ids`, which the edge already prints under the header.
