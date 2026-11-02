---
kind: added
---
- **The till voids a paid bill from Today's bills**
  ([ADR-0184](../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)). Under the chosen
  bill, **Void bill** asks for a manager's code and PIN and a reason, as the bill void before
  payment does, then says what to hand back in cash and what to reverse on the card terminal, and
  whether the void slip printed. A voided bill stays listed, marked **Voided**. A bill past the
  store's window, or with an issued e-invoice, is refused with a sentence that says where it is
  corrected. English and Vietnamese.
