#!/usr/bin/env bash
# Which template-by-country matrix runs a pull request needs (ADR-0200 decision 6).
#
# Reads the changed paths on stdin, one per line, and prints one `pos-simulator matrix` filter per
# line: `--template <dir>` for each template directory under business-types/ that changed (its row,
# in every country), and `--country <cc>` for each country pack under countries/ that changed (its
# column, every template). Prints nothing when neither changed. A schedule or a manual run passes no
# paths and runs the whole matrix instead, so this is only the pull request's half.
set -euo pipefail

paths="$(cat)"
{ grep -E '^business-types/[^/]+/' <<<"$paths" || true; } \
  | cut -d/ -f2 | sort -u | sed 's/^/--template /'
{ grep -E '^countries/[^/]+/' <<<"$paths" || true; } \
  | cut -d/ -f2 | sort -u | sed 's/^/--country /'
