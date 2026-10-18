---
kind: added
---
- **The Settings screen asks for a confirmation before it saves a value that needs one.** Picking a
  value that its setting's catalogue entry names in `confirm` shows a checkbox with the setting's
  notice, in English or Vietnamese, and says that the confirmation is recorded with the person's
  name and the time and covers the scope being written. Save stays disabled, and says why, until the
  box is ticked; the save then sends `notice_confirmed` (ADR-0169). No setting needs confirming yet.
