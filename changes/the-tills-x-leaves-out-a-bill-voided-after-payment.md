---
kind: changed
upgrade_note: The till's X (`GET /api/shifts/{id}/x`, `GET /api/shifts:x`) and a shift's close report now leave a bill voided after payment out of the sales, bill count, discounts, tax components and tenders of a shift that is still open (ADR-0258); routes and fields are unchanged, and the expected cash is the same as before. A shift closed before the void keeps its figures.
---
- **The till's X agrees with the console for the day.** A bill voided after payment used to stay in
  the shift's sales and tenders, beside its count under "voided after payment", so the till showed
  more sales than the console. It now leaves the open shift's figures, as the console leaves it out
  of the day (ADR-0258).
