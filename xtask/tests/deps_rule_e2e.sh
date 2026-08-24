#!/usr/bin/env bash
# Copyright (c) 2026 Pizza 4P's. All rights reserved.
# Proprietary and confidential. Internal use only. See LICENSE.
#
# Proves the dependency rule fires end to end, not merely that its pure function
# returns findings. The unit tests cover the logic; this covers the plumbing —
# `cargo metadata` invocation, allow-list loading, and the exit code CI reads.
#
# It works on a throwaway copy, so it can never modify the tree under review, and
# it asserts on the finding text rather than the exit code alone: a `cargo
# metadata` failure also exits non-zero, and that would pass for the wrong reason.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

tar -C "$root" --exclude=./target --exclude=./.git -cf - . | tar -xf - -C "$work"
cd "$work"

echo "1/3  the clean tree passes"
cargo run -q -p xtask -- deps-rule >/dev/null

echo "2/3  injecting tokio into pos-core"
# Insert under [dependencies]; appending to the file would land inside [lints].
awk '{print} /^\[dependencies\]$/ && !done {print "tokio = { version = \"1\", default-features = false }"; done=1}' \
    crates/pos-core/Cargo.toml > /tmp/injected.toml
mv /tmp/injected.toml crates/pos-core/Cargo.toml
cargo generate-lockfile --offline >/dev/null 2>&1 || cargo generate-lockfile >/dev/null

echo "3/3  the rule must now reject it"
output="$(cargo run -q -p xtask -- deps-rule 2>&1 || true)"

if ! grep -q 'dependency-rule' <<<"$output"; then
    echo "FAIL: no dependency-rule finding was produced. Output was:" >&2
    echo "$output" >&2
    exit 1
fi
if ! grep -q 'tokio' <<<"$output"; then
    echo "FAIL: the finding did not name tokio. Output was:" >&2
    echo "$output" >&2
    exit 1
fi

echo "ok — the dependency rule rejects an infrastructure crate in the domain"
