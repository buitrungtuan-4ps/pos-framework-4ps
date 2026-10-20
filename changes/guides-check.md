---
kind: added
---
- **`cargo xtask guides --check` keeps the guides true to the tree.** A guide in `docs/guides/` marks
  a block `guide:command`, `guide:verify` or `guide:excerpt <path>`. The check, in `just preflight`,
  fails when a guide's `cargo xtask new` command is refused, a verify step is not a cargo test, check
  or xtask command, quoted code is no longer verbatim in its file, or a marker is unknown or dangling.
