---
kind: changed
---
- **The till loads its store data through one engine, and says when part of it will not load**
  (ADR-0173). The floor, the price books, the button layout, the money settings and the reason
  codes are each declared once.
  - **Retried:** a read that fails is tried again on its own, from half a second up to fifteen.
  - **Held back:** after sign-in the till is drawn once the floor, the store's price book and the
    money settings have loaded. Until then it says what it is waiting for, with a **Retry**.
  - **Said out loud:** a missing button layout, walk-in price book or reason list shows a banner
    with a **Retry**. Void, discount, paid in, paid out and no-sale say beside them why they are
    disabled.
  - **Narrower re-reads:** a new configuration re-reads only what it changed, an unchanged read
    costs a `304`, and a reconnect re-reads everything.
