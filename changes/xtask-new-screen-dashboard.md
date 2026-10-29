---
kind: added
---
- **`cargo xtask new screen --app dashboard <id>` writes a console screen, wired everywhere a
  screen is named** (ADR-0170 decision 5). It writes the screen, with its title, and a vitest test
  that it draws that title. It adds one line each to:
  - the screen registry, with the screen's scope and icon;
  - the screen's nav group;
  - `App.tsx`, the `lazy()` block and the component entry;
  - every string catalogue.

  `--scope tenant|store|console`, `--icon` and `--nav-group` are required, because each is a design
  choice. The icon must already be one the console draws. A nav group that already holds six
  screens, the ceiling, is refused. Titles follow the till's rule: every catalogue other than
  English needs its `--title-<locale>`.
