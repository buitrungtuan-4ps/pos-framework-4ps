---
kind: added
---
- **New code must meet ADR-0170's size rules, and code that already breaks them may not grow**
  (the size ratchet, ADR-0170 decision 3).
  - `cargo xtask code-stats --check` runs in the `rules` job and in `just preflight`. These fail
    unless `tools/code-size-baseline.toml` lists them:
    - a Rust file over 1,000 lines, or 1,500 under `tests/`;
    - a function outside test code over 100 lines;
    - a function with more than 8 parameters.
  - A listed item may grow by 10 lines at most and by no parameters. One that is back within the
    rules, or gone, must be removed from the list, so the list only shrinks.
  - The list itself may only shrink. A pull request that adds an entry or raises a figure fails,
    unless its body says `size-baseline: grow <reason>`.
  - Items in changed files that pass the rules but exceed the warn levels are reported as
    warnings: a file over 800 lines (1,200 in tests), a function over 60 lines or with more than 7
    parameters.
  - `cargo xtask code-stats --write-baseline` writes the list. It starts with 36 files and 69
    functions.
  - The `code-stats` report gains each function's qualified name, such as `Edge::open_bill`.
  - The xtask checks find the repository from the checkout they run in
    (`git rev-parse --show-toplevel`), so worktrees that share a target directory no longer
    check each other's trees.
