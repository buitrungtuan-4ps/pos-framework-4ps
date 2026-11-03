---
kind: added
---
- **A package's conditions compile and evaluate in a restricted CEL.** The new `pos-rules` crate
  wraps `cel` 0.15 (no default features) and refuses, before a condition can run, a loop macro,
  a function off the allow-list, more than 1,024 characters, 256 nodes or a depth of 32, a
  fraction, and any name but the views `order`, `line`, `bill`, `channel`, `device`, `role` and
  `now` (ADR-0193). Amounts reach it as integers in minor units. Nothing calls it yet, so no store
  behaves differently.
