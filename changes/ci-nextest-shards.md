---
kind: changed
---
- **CI builds the tests once and runs them in three shards with cargo-nextest** (ADR-0171
  decision 2).
  - `test-archive` compiles the affected crates' tests into one nextest archive.
  - `test-shard` runs that archive in three partitions at once, each test in its own process,
    reporting every failure and retrying none.
  - `test-doc` runs the doc tests, which nextest does not run.
  - `test`, still the required check, passes only when all three did, or when no Rust was
    affected.
  - nextest is installed from its release binary, pinned and sha256-verified in
    `tools/tool-versions.toml`. Locally, `just test` keeps `cargo test`, and `CONTRIBUTING.md`
    gives the nextest command.
