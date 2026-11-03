---
kind: added
upgrade_note: Every shift answer from the edge gains `opening_float` (additive). The till's handover reads `handover_difference` and `handover_slip_print`, and its X reads `GET /api/shifts/{id}/x` and `GET /api/shifts:x`; against an edge older than ADR-0187 those fail and the till says so.
---
- **The till hands a shift over, reads an X, and says when the day is closed**
  ([ADR-0187](../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)). *Hand over the
  shift* (*Giao ca*) runs on the Shift screen from the outgoing blind count and close to the
  incoming sign-in, count and opening, asking one recount and then a confirmation of a count that
  differs, and printing the handover slip. An *X* button shows the shift's or the store's X with
  *Print X*; once every drawer of the day is closed the screen says *All tills closed for the day*
  with the store's figures. The cash in and out keypad shows at once, the close card comes before
  the next opening, and the drawer list shows each shift's own opening float.
