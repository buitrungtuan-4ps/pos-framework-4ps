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
preflight: fmt-check lint-config deps-rule vendor-neutral-core print-agent-deps tls-modes actions-pinned links mirrored-files clippy clippy-backbone test deny
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

# Run the store binary on fakes — no database, no hardware, no config file.
# Opens http://127.0.0.1:8787/ ; Ctrl-C stops it.
run-edge:
    cargo run -p minimal-edge

# Run pos_cloud locally, for a dev loop on the cloud tier. Unlike the edge it needs real
# backends — start them with `docker compose -f deploy/compose.yml up -d postgres nats garage`
# — and a config file named by POS_CLOUD_CONFIG (see docs/deploy-runbook.md). To bring the whole
# cloud up on one machine with Docker Compose instead, run deploy/bootstrap.sh (deploy/README.md).
run-cloud:
    POS_CLOUD_CONFIG="${POS_CLOUD_CONFIG:?set POS_CLOUD_CONFIG to a cloud.toml path; see docs/deploy-runbook.md}" cargo run -p pos-cloud

# Run the capacity model and the fleet scenarios (P12), printing the envelope and the
# reconciliation report. Deterministic and offline — no hardware, no clock.
simulate:
    cargo run -q -p pos-simulator

test:
    cargo test --workspace --locked
    cargo test --workspace --doc

# Needs the real backing services — not fakes — reachable and pointed at by env:
#   DATABASE_URL   e.g. host=localhost port=5432 user=pos password=pos dbname=poscloud
#   S3_ENDPOINT    e.g. http://localhost:9000  (+ S3_ACCESS_KEY / S3_SECRET_KEY)
#   NATS_URL       e.g. 127.0.0.1:4222
# Each adapter's tests are behind its `integration` Cargo feature. store-postgres shares one
# database so it runs single-threaded. This mirrors the merge-to-`main` `integration` job.
test-integration:
    cargo test -p store-postgres --features integration --locked -- --test-threads=1
    cargo test -p blob-garage --features integration --locked
    cargo test -p link-nats --features integration --locked

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

# Files duplicated across the ui/ and dashboard/ front-end build roots (design tokens, the contrast
# gate) must stay byte-identical — the substitute for a shared module they cannot import.
mirrored-files:
    cargo run -q -p xtask -- mirrored-files

# No external vendor's brand name may appear in pos-core / pos-proto production code — the
# automated half of the integration doctrine (ADR-0083).
vendor-neutral-core:
    cargo run -q -p xtask -- vendor-neutral-core

# The print agent links only the three workspace crates ADR-0112 allows — the rule that keeps one
# ESC/POS encoder in the tree, and domain code off a device that decides nothing.
print-agent-deps:
    cargo run -q -p xtask -- print-agent-deps

# bootstrap.sh picks a Caddyfile by name from the TLS_MODE accept-list (ADR-0090); keep the two in
# agreement, so a renamed or missing per-mode file is caught here and not on someone's box.
tls-modes:
    cargo run -q -p xtask -- tls-modes

# Regenerate the committed snapshots and generated docs from the code that owns them.
snapshot:
    POS_UPDATE_SNAPSHOTS=1 cargo test -q -p pos-proto snapshot
    POS_UPDATE_SNAPSHOTS=1 cargo test -q -p pos-core

# Refuse a removal from a committed snapshot, against the base branch.
snapshot-check base="origin/main":
    cargo run -q -p xtask -- snapshot --base {{base}}

# Refuse an edit to a shipped migration or a destructive statement (ADR-0017).
migrations-check base="origin/main":
    cargo run -q -p xtask -- migrations --base {{base}}

# ---------------------------------------------------------------------------
# Release. Signing is deliberately manual, from an offline key: there must be no
# path by which a compromised pipeline can ship software to the whole fleet
# (engineering-guide.md §6). This recipe therefore runs on a maintainer's machine,
# never on a runner.
# ---------------------------------------------------------------------------
sign:
    @echo "Signing is a manual step performed from the offline USB key." && exit 1

# Deploying the cloud is the GitHub Actions `deploy` workflow (Actions -> deploy -> Run
# workflow): CI builds the images and runs deploy/bootstrap.sh on your VPS over SSH, minting
# every secret on the box (ADR-0044). There is no laptop deploy — see docs/deploy-runbook.md.
deploy:
    @echo "Deploy is the GitHub Actions 'deploy' workflow. See docs/deploy-runbook.md."
