---
kind: changed
upgrade_note: A store built for Japan now rounds a bill's consumption tax once per rate rather than once per tax class (ADR-0226). A bill with two classes at one rate can come to ¥1 more or less tax than before, and its total is unchanged under tax-inclusive prices. Every other country computes exactly as before.
---
- **Japan's tax rounds once per rate per invoice** (ADR-0226 decision 1). The country pack states
  `tax_rounding_scope = PerRate`. The bill rounds each rate's tax once on the classes' combined
  amount and shares it back across the classes, so the lines still add up. Japan's conformance
  case 6 now comes to ¥183. Every other country still rounds per class.
- **Japan's and India's acceptance suites pin the phase 6 carry-overs** (ADR-0228). A comp lowers
  the taxable base, no scale-label layout is seeded, and a weight typed by hand is forbidden.
  These are Japan's cases 9 to 11 and India's cases 8 to 10. Each is still to be confirmed by
  legal or the accountant.
