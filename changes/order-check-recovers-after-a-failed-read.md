---
kind: fixed
---
- **The order's total recovers after a failed read.** One read of the order screen's total that
  failed left it at "—" for as long as the screen stayed open, however many later reads answered.
  A failed read now shows the dash until the next change reads it again (`docs/ui-ux.md` §3).
