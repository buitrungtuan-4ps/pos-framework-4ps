---
kind: added
upgrade_note: The rollup blob gains the bills each day settled; it fills from the projector's cursor forward. A bill settled before the upgrade and voided after it stays in its day's totals until the rollup is reset (the ADR-0036 reset-cursor-and-replay lever), which re-folds the whole log.
---
- **Reports leave out a bill voided after payment, and count it apart**
  ([ADR-0184](../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)). The cloud's
  revenue rollup takes a voided bill's sales, fees, tax and tax components back out of the day it
  was sold on and counts it under `voided_after_payment` (bills and amount); the cash rollup shows
  the cash handed back as `voided_cash`, for the store and for the drawer of the void's shift. The
  X/Z report and the console's Reports screen show both. Per store only: ADR-0184's second
  amendment drops the per-employee void count.
