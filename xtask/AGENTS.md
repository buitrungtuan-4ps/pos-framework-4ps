# `xtask/` — local rules

Read the root [`AGENTS.md`](../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** the repository's own checks, run as `cargo xtask <check>`. See
[`README.md`](README.md).

## Local rules

- A check returns findings. It returns `Err` only when it could not run at all, so one broken rule
  never hides the next.
- A finding names the file, and the line where it can, and says what to do. The person reading it
  should not have to open this crate.
- The rule is a pure function over its inputs (`check(…)`), with tests that drive it from fixtures
  and show it failing. A check nobody has watched fail is not a check.
- A new check is wired in three places: `src/main.rs`, the `justfile` (and `preflight`, if it is
  fast), and the `rules` job in `.github/workflows/pr.yml`.
- xtask reads the tree and `cargo metadata`. It needs no network, and it adds no dependency
  without an ADR.

## Most often touched

`src/main.rs` · `src/checks/<check>.rs` · `src/checks/mod.rs` · `src/gen_docs.rs` · `src/code_stats/`

## Gates

`cargo test -p xtask` · `xtask/tests/deps_rule_e2e.sh` (the `rules` job) · workspace clippy.

## Read first

[`docs/engineering-guide.md`](../docs/engineering-guide.md) §4 ·
[ADR-0170](../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) ·
[ADR-0171](../docs/adr/0171-ci-runs-what-a-change-affects.md).
