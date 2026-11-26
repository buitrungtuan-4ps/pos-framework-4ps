---
kind: added
---
- **`einvoice.number_source` offers `EINVOICE_NUMBER_SOURCE_HYBRID`, behind a recorded
  confirmation** (ADR-0251 decision 6). The store numbers from its own offline symbol while it
  cannot reach the cloud, and lets the vendor number while it can. Because it is allowed only if
  legal and the vendor confirm, the console asks the person setting it to confirm that both have,
  and the cloud records who did (ADR-0169). HQ gives the store its offline symbol on the
  E-invoices screen (**Allocate an offline symbol**), from the same ledger as every symbol, so the
  store's and the vendor's sequences never meet. Without one, a `HYBRID` store waits for the vendor
  offline too. The default stays `EINVOICE_NUMBER_SOURCE_POS`.

  **Upgrade note (protocol, API):** the `einvoice` node gains `offline_symbol`, and
  `POST /admin/config/einvoice-symbol/allocate` takes an optional `offline`. Both are additive.
