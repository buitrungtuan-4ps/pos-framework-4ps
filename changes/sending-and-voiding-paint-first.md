---
kind: changed
---
- **Sending to the kitchen and voiding a line show at once.** The lines show *Fired*, or the line
  *Voided*, on the tap, marked as sending until the store server answers. A refusal puts them back
  with its sentence, and a send or void with no answer puts them back and reads the open orders
  again, since the store server may have made it; the till never sends it again. Every edge write
  the till paints first now takes one path.
