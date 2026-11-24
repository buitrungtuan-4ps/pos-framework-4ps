---
kind: added
---
- **Vietnam's legal acceptance suite** (roadmap-v4 phase 7). `countries/vn/tests/conformance.rs`
  gains cases 8 to 14:
  - a comp lowers the taxable base;
  - no scale-label layout is seeded;
  - a weight typed by hand is forbidden (ADR-0228);
  - an e-invoice sequence is gapless and restarts with the year;
  - a replacement box never repeats a number;
  - an invoice states each rate once and adds up to the bill;
  - every sale issues an invoice by default (ADR-0225).

  `country_conformance::comp_failure` runs a case with a comp. `countries/vn/CONFORMANCE.md` names
  the suite and maps R21 to R23. Every legal point is still to be confirmed.
