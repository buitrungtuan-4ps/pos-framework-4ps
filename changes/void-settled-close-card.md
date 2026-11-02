---
kind: added
---
- **A shift's close says what voids after payment handed back**
  ([ADR-0184](../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)). The till's close
  card and the printed shift report show how many paid bills were voided at the till during the
  shift and the cash they handed back out of the drawer, which the expected amount leaves out, so
  the report's figures add up again. The closed shift carries it as `voided_after_payment`
  (`bills`, `cash_returned`), absent before the close and where there was none. English and
  Vietnamese.
