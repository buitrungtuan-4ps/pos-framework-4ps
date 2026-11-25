---
kind: added
---
- **A store's e-invoice symbol is allocated by itself** where `einvoice.symbol_allocation` is
  `EINVOICE_SYMBOL_ALLOCATION_AUTOMATIC`, as Vietnam's pack seeds (ADR-0240). Redeeming an activation
  code for a store with no symbol allocates its first, and a lease bump for a replacement box
  publishes a new one in the same version as the lease. Both are audited as the system, and the
  bump's audit entry names the symbol. The console's **E-invoices** screen gains **Allocate the next
  symbol**.
