---
kind: changed
---
- **Opening, counting and closing a till's own shift wait for the store server.** The till refuses
  an opening while it holds a shift not yet closed, and a float or count whose notes do not add up,
  before sending anything. It shows the act as pending until the store server answers, shows a
  refusal in place for the cashier to dismiss, and, with no answer, never sends again: it reads the
  drawer's shift until it can tell what was made, and then shows the store server's figures rather
  than what the till typed.
