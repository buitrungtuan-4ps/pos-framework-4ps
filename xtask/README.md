# `xtask`

**Layer** `tool` · **Owners** @maintainers-architecture

## What it is

The repository's own checks, run as `cargo run -q -p xtask -- <name>`, which is what CI and the `justfile` recipes call. Each check reads the tree, or the diff against a base branch, and fails with a finding that names the file and, where it can, the fix. It is not part of either binary.

## Owns

- The `rules` CI job's checks: deps-rule, lint-config, actions-pinned, links, countries, crates, mirrored-files, vendor-neutral-core, print-agent-deps, tls-modes, snapshot, migrations
- `changelog`: checks and folds `changes/`
- `code-stats`: measures file, function and parameter sizes. Test code has the looser test limits. Test code is:
  - a file under a `tests/` or `benches/` directory;
  - the file of a module a `#[cfg(test)]` gates;
  - the one item a `#[cfg(test)]` gates, which is the whole module when that item is a `mod`.

  A gated `use` or a single gated function makes nothing else test code.

## Depends on / used by

- **Depends on** (workspace crates): no workspace crate.
- **Used by** (workspace crates): no workspace crate.

## Entry points

- `src/main.rs`: the subcommand table
- `src/checks/<name>.rs`: one file per check
- `src/code_stats/`: the size measurements
- `src/finding.rs`: the finding a check reports

## Rules

- [ADR-0017](../docs/adr/0017-migrations.md) · [ADR-0111](../docs/adr/0111-a-second-origin-may-address-the-edge.md) · [ADR-0170](../docs/adr/0170-repository-layout-code-naming-and-size-rules.md)

## How to test

```bash
cargo test -p xtask
```
