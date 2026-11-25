---
kind: fixed
---
- **An e-invoice adjustment can no longer take the invoice below zero.** The console's
  **E-invoices → Correct** accepted an adjustment of −999,999 ₫ on a 205,000 ₫ invoice. The cloud
  now counts every correction already recorded against the invoice (a replacement sets its total;
  an adjustment moves it) and refuses one that would leave it under nothing (`400`, `OUT_OF_RANGE`
  on `total_minor`). Whether an adjustment may ever exceed what was sold is to be confirmed by legal
  or the accountant.
