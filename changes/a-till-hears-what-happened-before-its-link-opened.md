---
kind: fixed
---
- **A till no longer misses what happens just after it is paired.** A till's live link is refused
  until the device pairs, and its retry can come after the till has signed in and read the store.
  Whatever another screen did in between — a table seated, an order started — never reached it, so
  its floor could show a taken table as free until the page was reloaded. A till's first link now
  asks the store server for what it cannot name, and reads again what is open when the server says
  it cannot replay it, which is every first connect. No route, setting or protocol changes: it is
  the same answer a link that dropped before its first event already got. While it reads again, what
  the link carries meanwhile is applied after the read rather than under it, so an older answer no
  longer puts a just-seated table back to free.
