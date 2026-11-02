---
kind: added
---
- **The store voids a paid bill whole** ([ADR-0184](../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)).
  `POST /api/bills/{id}/void_settled` takes a reason from the `VOID_BILL` list and, where the person
  does not hold `billing.bill.void_settled` directly, a holder's code and PIN. It refuses a bill
  outside the store's `billing.settled_void_window` (`SETTLED_VOID_WINDOW_CLOSED`) and one with an
  issued e-invoice (`E_INVOICE_ISSUED`; none is issued yet). The bill's cash leaves the drawer of
  the till the void is done at, whose expected cash falls by exactly that, and the drawer opens;
  card and other tenders are recorded as reversed; tips are untouched. The void and its approval
  commit together, stamped with that drawer's shift, so a restart rebuilds both. ADR-0184 gains an
  amendment: the void slip names no person, and the edge reports no e-invoice until stores issue
  them.
