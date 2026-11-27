---
kind: fixed
---
- **The console's X/Z tender totals leave out a bill voided after payment.** Sales, tax and `net`
  already left it out (ADR-0184 decision 9), but `by_tender` kept its payments, so the tenders added
  up to more than the day's takings. A void now takes each of the bill's payments out of its day's
  tender totals, cash and delivery-app payments alike. A bill kept before this change, voided later,
  keeps its tenders until the rollup is reset (ADR-0036).
