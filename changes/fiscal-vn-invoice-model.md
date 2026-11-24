---
kind: added
---
- **Vietnam's e-invoice model** (ADR-0225). `pos-country-vn` now holds the invoice symbol, checked
  for shape, with seller characters for each issuing point. It also holds the gapless eight-digit
  sequence that restarts with each year's symbol, the original, adjustment and replacement kinds,
  the `einvoice.issue_moment` choices with every sale recommended, the buyer check with its mã số
  thuế, and the per-rate totals check. Each is mapped in `countries/vn/CONFORMANCE.md` R15–R20, and
  every one is still to be confirmed by legal.
