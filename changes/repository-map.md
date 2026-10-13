---
kind: added
---
- **A documentation index, a repository map, and code naming rules** (ADR-0170 decisions 1 and 2).
  - `docs/README.md` files every document under Tutorials, How-to guides, Reference or Explanation,
    with a line on what each is for.
  - `docs/repository-map.md` lists, for each kind of change (a domain rule, an edge or cloud route,
    a setting, a permission, an event, a screen, an adapter, a country, a migration, a CI job, a
    release), the files to edit and the gate that checks each. It also says where one change still
    has to be made in several places.
  - `docs/naming-and-api.md` §10.1 states the rules for code and files: the Rust API Guidelines,
    `PascalCase.tsx` components and `kebab-case.ts` modules, ISO and BCP 47 codes, and English in
    code. Each rule has an example from the tree.
  - The root `README.md` and `AGENTS.md` link both. `AGENTS.md` §5 now names `countries/<cc>/` for
    country law, and `dashboard/` for console screens.
