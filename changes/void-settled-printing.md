---
kind: added
---
- **A paid bill's void prints a slip, and its receipt's copy says VOID**
  ([ADR-0184](../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)). After
  `POST /api/bills/{id}/void_settled` commits, the till's receipt printer prints **BILL VOIDED**
  under the receipt's number, with the amount, the reason, the till, the time, the cash handed
  back and, for a card or another tender, the amount to reverse on the card terminal; it names no
  person. The answer says what came of it (`void_slip_print`). `GET /api/bills/settled` keeps a
  bill voided after payment, marked `voided`, and its copy prints **VOID** above **COPY**. The
  paper's words are in English and Vietnamese.
