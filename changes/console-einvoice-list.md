---
kind: added
upgrade_note: A new console permission, `console.einvoices.correct`, is granted to OWNER and ADMIN. Migration 0107 adds a unique index on an e-invoice's number per tenant (additive).
---
- **The console lists a store's e-invoices and corrects them** (ADR-0241 decision 5). The
  **E-invoices** screen shows each invoice's number, date, kind, total and submission state, with
  the provider's reference or its reason for a refusal. **Correct** records an adjustment or a
  replacement, numbered under the tenant's correction symbol and submitted like any other invoice.
  No buyer is shown.
