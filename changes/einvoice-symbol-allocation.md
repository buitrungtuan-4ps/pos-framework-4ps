---
kind: added
upgrade_note: Migration 0105 adds the `einvoice_symbols` table (additive). A new setting, `einvoice.symbol_allocation`, defaults to `EINVOICE_SYMBOL_ALLOCATION_MANUAL`; a new Vietnamese store's pack seeds `_AUTOMATIC`. `PUT /admin/config/einvoice-symbol` now answers `409 E_INVOICE_SYMBOL_IN_USE` for a symbol it already gave another store of the tenant; symbols given before this release are not in the ledger and are not checked.
---
- **The cloud allocates a store's e-invoice symbol** (ADR-0240). `POST /admin/config/einvoice-symbol/allocate`
  gives a store the next symbol of its country's rule that no store of the tenant ever held, and
  publishes it. Vietnam's rule is `1C<yy>M` and a seller pair from `AA` to `ZZ`; the letters are to
  be confirmed by legal. Every symbol given, by hand or allocated, is kept in a ledger for good, and
  the hand route refuses a symbol another store holds.
