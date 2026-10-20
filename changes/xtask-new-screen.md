---
kind: added
---
- **`cargo xtask new screen --app ui <id> --path <path> --title <English>` writes a till screen,
  wired everywhere a screen is named** (ADR-0170 decision 5). It writes the screen file, with its
  title, and adds one line each to:
  - the screen registry;
  - `App.tsx`, the import and the component entry;
  - the English and Vietnamese string catalogues;
  - the replay test's route walk.

  Every catalogue other than English needs its title: `--title-vi` today, and `--title-<locale>` for
  any catalogue added later. The generator refuses to run without one, so it never writes a string
  nobody gave it.
