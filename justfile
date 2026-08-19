# The commands AGENTS.md §3 promises. `just preflight` is the only one a
# contributor needs before opening a pull request.
#
# CI calls `cargo xtask <check>` directly rather than going through `just`, so the
# pipeline does not depend on `just` being installed on a runner.

set shell := ["bash", "-euo", "pipefail", "-c"]
export CARGO_TERM_COLOR := "always"

default: preflight

# ---------------------------------------------------------------------------
# The gate. Ordered so the cheapest failure surfaces first: a contributor with a
# naming mistake should not wait for a full clippy pass to hear about it.
# ---------------------------------------------------------------------------
preflight: fmt-check lint-config deps-rule actions-pinned links clippy clippy-backbone test deny
    @echo "preflight ok — ready for a pull request"

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

# Workspace pass, then the backbone pass. The second one is not redundant: a
# source-level `#![allow(clippy::unwrap_used)]` overrides a `deny` from
# [workspace.lints], so the table in Cargo.toml is not a floor. `--forbid` cannot
# be overridden — `allow` against `forbid` is error E0453 — which is what makes
# the backbone's no-panic rule unescapable rather than merely conventional.
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

clippy-backbone:
    for crate in pos-core pos-ports pos-proto; do \
        cargo clippy -p "$crate" --all-targets -- \
            -F clippy::unwrap_used -F clippy::expect_used -F clippy::panic \
            -F clippy::todo -F clippy::unimplemented \
            -F clippy::print_stdout -F clippy::print_stderr \
            -F clippy::float_arithmetic -F unsafe_code -D warnings; \
    done

build:
    cargo build --workspace --locked

test:
    cargo test --workspace --locked
    cargo test --workspace --doc

# Needs Docker: real PostgreSQL and NATS, not fakes.
test-integration:
    cargo test --workspace --locked --test '*' -- --include-ignored

deny:
    cargo deny --all-features check advisories licenses bans sources

# ---------------------------------------------------------------------------
# xtask-backed checks.
# ---------------------------------------------------------------------------
deps-rule:
    cargo run -q -p xtask -- deps-rule

# Proves the dependency rule fires end to end, on a throwaway copy of the tree.
deps-rule-e2e:
    bash xtask/tests/deps_rule_e2e.sh

lint-config:
    cargo run -q -p xtask -- lint-config

actions-pinned:
    cargo run -q -p xtask -- actions-pinned

links:
    cargo run -q -p xtask -- links
    cargo run -q -p xtask -- countries

# Regenerate the committed snapshots and generated docs from the code that owns them.
snapshot:
    POS_UPDATE_SNAPSHOTS=1 cargo test -q -p pos-proto snapshot
    POS_UPDATE_SNAPSHOTS=1 cargo test -q -p pos-core

# Refuse a removal from a committed snapshot, against the base branch.
snapshot-check base="origin/main":
    cargo run -q -p xtask -- snapshot --base {{base}}

# ---------------------------------------------------------------------------
# Development loops. These arrive with the phase that creates their binary
# (docs/roadmap.md) and fail with a clear message until then.
# ---------------------------------------------------------------------------
run-edge:
    @echo "pos-edge arrives in P5 (docs/roadmap.md)." && exit 1

run-cloud:
    @echo "pos-cloud arrives in P7 (docs/roadmap.md)." && exit 1

simulate:
    @echo "pos-simulator arrives in P12 (docs/roadmap.md)." && exit 1

# ---------------------------------------------------------------------------
# Release. Signing is deliberately manual, from an offline key: there must be no
# path by which a compromised pipeline can ship software to the whole fleet
# (engineering-guide.md §6). This recipe therefore runs on a maintainer's machine,
# never on a runner.
# ---------------------------------------------------------------------------
sign:
    @echo "Signing is a manual step performed from the offline USB key." && exit 1

deploy:
    @echo "deploy/ arrives in P8 (docs/roadmap.md)." && exit 1
