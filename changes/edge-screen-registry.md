---
kind: changed
---
- **The till's screens are one table** (ADR-0170 decision 5), as the console's already are.
  - `ui/src/state/screens.ts` lists every routed screen: its path and, where it has one, its
    title's string.
  - The router is built from that table, and `App.tsx` maps each screen to its component in a
    `Record` keyed by screen, so a screen without a component, or a component without a screen,
    does not compile.
  - The step budget reads its routes from the table.
  - A replay test opens every route and checks it shows its own screen.

  No route, screen or behaviour changes.
