---
kind: added
---
- **A free table asks how many guests.** Tapping a free table on the floor opens a pad of counts;
  one tap seats the table with that count and opens its order, so seating is still two taps. Skip
  seats it without a count, as before, and Cancel leaves it free. A store whose package makes the
  guest count required gets no Skip, and the first dish waits for the count: a table seated
  without one says so and holds the menu, and the store server refuses the dish too. The store
  server's `GET /api/package` now carries the running version's flows for the till (additive).
