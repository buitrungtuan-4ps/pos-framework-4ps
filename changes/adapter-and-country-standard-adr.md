---
kind: added
---
- **ADR-0176: the adapter and country standard.** New adapters and countries are to be generated
  by `cargo xtask new adapter --kind printer|storage|card|fiscal` and `cargo xtask new country <cc>`
  rather than copied by hand. A generated crate passes its port's contract suite from its first
  commit, names no vendor, and takes every legal value of a country as a required flag. The commands
  themselves land in the pull requests that follow.

  Nothing a store or a console user sees changes.
