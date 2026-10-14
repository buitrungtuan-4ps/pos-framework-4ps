---
kind: changed
---
- **The till loads who is signed in, the drawers, the shift and the open orders through the same
  engine as its configuration** (ADR-0173).
  - **Held back:** a till that reloads is drawn once the session and the open orders have loaded,
    and says so with a **Retry** when they cannot be. It no longer draws an empty order over live
    food.
  - **Retried:** a failed read of the shift or the drawers is tried again on its own.
  - **One session read:** a reload reads the session once, where it read it twice.
  - **Re-read when it matters:** a `resync` re-reads the shift as well.
