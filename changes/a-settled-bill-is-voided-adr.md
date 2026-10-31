---
kind: added
---
- **ADR-0184, proposed: a settled bill is voided whole, not refunded.** A wrong charge is corrected by
  a manager voiding the paid bill (PIN and reason, void slip printed) and ringing a new one; there is
  no partial refund. Cash leaves that till's drawer, a card charge is reversed at the card terminal,
  the receipt number stays used. Allowed on the bill's business date by default, or only while its
  shift is open, as a store setting; refused while an e-invoice is issued for the bill. The code
  follows in its own change.
