#!/usr/bin/env bash
# Runs bootstrap.sh's cloud.toml reconcile block against fixture files.
#
# WHY THIS EXISTS. Five consecutive deploys were blocked inside `deploy/bootstrap.sh`, and CI ran
# none of it: the file is shipped to a box over SSH and executed there, so every gate in `pr.yml`
# was green while the script could not complete. Four of the five were the same shape — the gap
# between a fresh CI checkout and a real box that has bootstrapped before — and the fifth was a
# plain shell bug in the fix for the fourth:
#
#   * a `cloud.toml` that step 2 had chowned to uid 10001, which step 7b then could not write;
#   * `internal_shared_secret` having no upgrade path, so the cloud crash-looped after ADR-0097;
#   * the reconciles reporting `set` for keys they had never written;
#   * and then `cloud_toml_mint_key_if_absent … ; case "$?"`, which under `set -e` exits before the
#     `case` is reached — so answering "already there" *killed bootstrap*, on every box that had
#     the key. Deploy #19 died on it, after the reconcile above it had just succeeded.
#
# None of those needed a VPS to catch. They needed the block to be RUN, over the file states a real
# box presents. That is all this does: extract the helper region and the reconcile block verbatim
# from bootstrap.sh, run them against a fixture `secrets/cloud.toml`, and assert both the exit
# status and what was written where.
#
# It extracts rather than duplicates deliberately — a transcribed copy is a copy that drifts, and a
# test that passes against a copy of the old code is worse than no test. If the extraction stops
# matching the file the assertions below fail loudly rather than vacuously passing.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BOOTSTRAP="$HERE/../bootstrap.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fails=0
fail() { printf 'FAIL  %s\n' "$1" >&2; fails=$((fails + 1)); }
pass() { printf 'ok    %s\n' "$1"; }

# --------------------------------------------------------------------------------------------------
# Extract the two regions, and refuse to run if either came out empty or incomplete.
# --------------------------------------------------------------------------------------------------
# `|| true`: a `grep` that matches nothing fails, and under `set -o pipefail` that failure
# propagates out of the substitution and kills the assignment. An empty answer is a real
# answer here — the caller checks for it and says something useful.
line_of() { grep -n "$1" "$BOOTSTRAP" | head -1 | cut -d: -f1 || true; }

h_start="$(line_of '^# 1c\.')"
h_end=$(( $(line_of '^# 1\. PostgreSQL credentials\.') - 1 ))
c_start="$(line_of '^  qr_note=')"
c_end=$(( $(awk -v s="$c_start" 'NR>s && /^fi$/ {print NR; exit}' "$BOOTSTRAP") - 1 ))

[ -n "$h_start" ] && [ -n "$c_start" ] && [ "$h_end" -gt "$h_start" ] && [ "$c_end" -gt "$c_start" ] || {
  echo "FAIL  could not locate the helper region or the reconcile block in bootstrap.sh" >&2
  echo "      the markers this test keys on have moved; re-point line_of() above" >&2
  exit 1
}

{
  echo 'set -euo pipefail'
  echo 'SECRETS="$PWD/secrets"'
  sed -n "${h_start},${h_end}p" "$BOOTSTRAP"
  sed -n "${c_start},${c_end}p" "$BOOTSTRAP"
  echo 'echo "done   bootstrap complete"'
} > "$WORK/reconcile.sh"

for fn in cloud_toml_reader cloud_toml_writer cloud_toml_state cloud_toml_append \
          cloud_toml_key_is_top_level cloud_toml_mint_key_if_absent; do
  grep -q "^$fn() {" "$WORK/reconcile.sh" || fail "extraction lost $fn()"
done
for call in 'cloud_toml_mint_key_if_absent table_token_secret' \
            'cloud_toml_mint_key_if_absent internal_shared_secret' \
            'cloud_toml_key_is_top_level internal_shared_secret'; do
  grep -q "$call" "$WORK/reconcile.sh" || fail "extraction lost the call: $call"
done
bash -n "$WORK/reconcile.sh" || fail "the extracted region does not parse"
[ "$fails" = 0 ] || { echo "refusing to assert against a broken extraction" >&2; exit 1; }
pass "extracted the helpers and the reconcile block from bootstrap.sh"

# --------------------------------------------------------------------------------------------------
# Scenarios. Each writes a fixture cloud.toml, runs the block, and checks status + output.
# --------------------------------------------------------------------------------------------------
cd "$WORK"

scenario() {   # $1 = name; stdin = the fixture cloud.toml
  name="$1"
  rm -rf secrets && mkdir -p secrets
  cat > secrets/cloud.toml
  set +e
  out="$(bash "$WORK/reconcile.sh" 2>&1)"
  rc=$?
  set -e
}

expect_completed() {
  [ "$rc" = 0 ] || { fail "$name: exited $rc (bootstrap would have stopped here)"; printf '%s\n' "$out" | sed 's/^/      /' >&2; return; }
  case "$out" in
    *"done   bootstrap complete"*) pass "$name: completed" ;;
    *) fail "$name: never reached the end of the block"; printf '%s\n' "$out" | sed 's/^/      /' >&2 ;;
  esac
}
expect_says()     { case "$out" in *"$1"*) pass "$name: said \"$1\"" ;; *) fail "$name: did not say \"$1\""; printf '%s\n' "$out" | sed 's/^/      /' >&2 ;; esac; }
expect_silent_on(){ case "$out" in *"$1"*) fail "$name: should not have mentioned \"$1\""; printf '%s\n' "$out" | sed 's/^/      /' >&2 ;; *) pass "$name: silent on \"$1\"" ;; esac; }

# The key must land ABOVE the first table header, which is the only place the cloud reads it: step
# 7b appends `[artifacts]`, so a key appended to the end parses as `artifacts.<key>` — still at the
# start of its line, still found by a bare `grep -q`, and never read.
expect_top_level() {
  k="$(grep -n "^$1" secrets/cloud.toml | head -1 | cut -d: -f1 || true)"
  t="$(grep -n '^\[' secrets/cloud.toml | head -1 | cut -d: -f1 || true)"
  if [ -z "$k" ]; then fail "$name: $1 was not written at all"
  elif [ -n "$t" ] && [ "$k" -ge "$t" ]; then fail "$name: $1 landed at line $k, at or below the first table header at $t"
  else pass "$name: $1 is top-level (line $k, first table at ${t:-none})"
  fi
}

# --- A. Every key already there. THE REGRESSION: this is every box that has bootstrapped since the
#        template carried them, and the "already there" answer used to kill the script.
#
#        Add a key to the template and it belongs here too, or this scenario stops meaning "a fully
#        reconciled box" and starts meaning "a box missing the newest key" — which is scenario B.
scenario "A every key present" <<'EOF'
bind = "0.0.0.0:8080"
internal_shared_secret = "aaaa"
table_token_secret = "bbbb"
archive_key_secret = "cccc"

[artifacts]
bucket = "pos-artifacts"
EOF
expect_completed
expect_silent_on "set    cloud.toml"

# --- B. Deploy #19's exact path: one key to mint, one already present.
scenario "B one to mint, one present" <<'EOF'
bind = "0.0.0.0:8080"
internal_shared_secret = "aaaa"

[artifacts]
bucket = "pos-artifacts"
EOF
expect_completed
expect_says "set    cloud.toml table_token_secret"
expect_top_level table_token_secret

# --- C. A box that predates both keys — the upgrade run has to add them, in the right place.
scenario "C neither present" <<'EOF'
bind = "0.0.0.0:8080"

[artifacts]
bucket = "pos-artifacts"
EOF
expect_completed
expect_says "set    cloud.toml table_token_secret"
expect_says "set    cloud.toml internal_shared_secret"
expect_top_level table_token_secret
expect_top_level internal_shared_secret

# --- D. A person appended the fatal key by hand, which lands inside [artifacts]. bootstrap cannot
#        fix it (it never rewrites a secret it finds) but it must say so — pos_cloud will not start.
scenario "D internal_shared_secret misplaced by hand" <<'EOF'
bind = "0.0.0.0:8080"
table_token_secret = "bbbb"

[artifacts]
bucket = "pos-artifacts"
internal_shared_secret = "wrong-place"
EOF
expect_completed
expect_says "REFUSE TO START"

# --- E. No table header at all: every key counts as top-level, and the append path runs instead of
#        the sed path.
scenario "E no table header" <<'EOF'
bind = "0.0.0.0:8080"
EOF
expect_completed
expect_top_level table_token_secret
expect_top_level internal_shared_secret

# --- F. The file cannot be read: "could not tell" must be its own answer, never rendered as "set",
#        and must not stop the run. Needs a shell that is neither root nor able to sudo.
rm -rf secrets stub && mkdir -p secrets stub
printf 'bind = "0.0.0.0:8080"\n\n[artifacts]\nb = 1\n' > secrets/cloud.toml
printf '#!/bin/sh\nexit 1\n' > stub/sudo && chmod +x stub/sudo   # `command -v sudo` finds it; it always fails
name="F unreadable cloud.toml"
if [ "$(id -u)" = 0 ]; then
  chmod 600 secrets/cloud.toml && chown 10001:10001 secrets/cloud.toml
  chmod 755 "$WORK" secrets
  set +e
  out="$(setpriv --reuid=10002 --regid=10002 --clear-groups \
           env PATH="$WORK/stub:$PATH" bash "$WORK/reconcile.sh" 2>&1)"
  rc=$?
  set -e
else
  chmod 000 secrets/cloud.toml
  set +e
  out="$(env PATH="$WORK/stub:$PATH" bash "$WORK/reconcile.sh" 2>&1)"
  rc=$?
  set -e
  chmod 600 secrets/cloud.toml
fi
expect_completed
expect_says "cannot tell whether internal_shared_secret is set"
expect_silent_on "set    cloud.toml"

# --------------------------------------------------------------------------------------------------
# The static rule the fifth blocker earned: read a helper's status through a captured variable, not
# through `$?`. `cmd || rc=$?` is exempt from `set -e` and captures in one step; a bare `cmd`
# followed by `case "$?"` exits the script before the `case` runs. The comment on line 111 of
# bootstrap.sh is allowed to mention `$?`; code is not.
# --------------------------------------------------------------------------------------------------
name="G no bare \$? reads"
offenders="$(grep -n '\$?' "$HERE"/../*.sh | grep -v '^\([^:]*\):\([0-9]*\):[[:space:]]*#' | grep -v '|| [a-z_]*_rc=\$?' || true)"
if [ -n "$offenders" ]; then
  fail "$name: read a status through \$? instead of \`|| rc=\$?\`"
  printf '%s\n' "$offenders" | sed 's/^/      /' >&2
else
  pass "$name"
fi

printf '\n'
if [ "$fails" = 0 ]; then echo "deploy/tests/cloud-toml-reconcile.sh: all checks passed"; else echo "$fails check(s) failed" >&2; exit 1; fi
