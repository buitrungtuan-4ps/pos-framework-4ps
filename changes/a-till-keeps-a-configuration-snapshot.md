---
kind: added
---
- **The till keeps a snapshot of the store's configuration, and only that** (ADR-0173).
  - **What it holds:** the price books, the button layout, the money settings and the reason codes,
    each with the edge's tag, in the browser's IndexedDB.
  - **Faster restarts:** a restart sends each tag and is answered `304` where nothing changed.
  - **Confirmed before use:** a price book or the money settings are used only once the edge
    confirms them.
  - **Shown at once:** the layout and the reasons appear immediately, even if the edge cannot be
    reached.
  - **Never kept:** the floor, the session, permissions, tokens, staff or customer data, drawers,
    shifts or orders.
  - **Deleted:** on sign-out and on unpairing, and unused when it comes from another store server
    or pairing, or is older than seven days.
  - **No storage:** where IndexedDB is unavailable, the till works without a snapshot.
