---
kind: fixed
---
- **A change sent a moment before the page reloads is not lost.** A reload or a navigation used to
  cancel a till's request still on its way to the store server, and the server dropped it before it
  was recorded: a send to the kitchen or a bump made just before could vanish. Every change is now
  sent so that leaving the page does not cancel it.
