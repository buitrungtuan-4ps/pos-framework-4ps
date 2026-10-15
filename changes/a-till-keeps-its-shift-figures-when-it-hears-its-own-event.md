---
kind: fixed
---
- **A till that opens or counts a shift keeps what the drawer should hold.** Where the store's count
  is not blind, the expected figure could vanish from the Shift screen just after the shift opened,
  when the till heard its own `cash.shift.opened` after the store server's answer: the event carries
  no figures, and it replaced the shift the answer had filled in. An event about the shift the till
  already shows now moves only its state, and its count; a shift the till does not yet show is read
  back for its figures.
