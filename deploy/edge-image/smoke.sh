#!/usr/bin/env bash
# Smoke test for the edge container image (ADR-0113): builds it around a pos_edge binary, checks the
# binary reads a store's configuration from its volume (`--self-test`), starts it, waits for
# `/healthz`, stops it with SIGTERM and checks it exits 0. Fake store id, no cloud, nothing secret.
#
#   deploy/edge-image/smoke.sh target/debug/pos-edge
set -euo pipefail

binary="${1:?usage: smoke.sh <path to a linux pos-edge binary>}"
here="$(cd "$(dirname "$0")" && pwd)"
tag="pos-edge:smoke"
name="pos-edge-smoke-$$"
context="$(mktemp -d)"
state="$(mktemp -d)"
cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  rm -rf "$context"
  # The container wrote as its own uid; remove what it left through a container of the same image.
  docker run --rm --user 0 --entrypoint /bin/rm -v "$state:/s" "$tag" -rf /s/store.sqlite /s/store.sqlite-wal /s/store.sqlite-shm >/dev/null 2>&1 || true
  rm -rf "$state" 2>/dev/null || true
}
trap cleanup EXIT

cp "$here/Dockerfile" "$context/"
cp "$binary" "$context/pos-edge"
docker build -q -t "$tag" "$context" >/dev/null

# A fake store: the id is a fixed test ULID, and the edge listens on every interface of its own
# network namespace only.
cat > "$state/config.toml" <<'TOML'
store_id = "01J0000000000000000000000A"
bind = "0.0.0.0:8787"
store_path = "/var/lib/pos-edge/store.sqlite"
TOML
# The container runs as its own uid, which must be able to create the database beside the config.
chmod 0777 "$state"
chmod 0644 "$state/config.toml"

docker run --rm -v "$state:/var/lib/pos-edge" "$tag" --self-test

docker run -d --name "$name" --read-only --cap-drop ALL --security-opt no-new-privileges:true \
  --tmpfs /tmp -v "$state:/var/lib/pos-edge" -p 127.0.0.1::8787 "$tag" >/dev/null
port="$(docker port "$name" 8787/tcp | head -n1 | sed 's/.*://')"
for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:${port}/healthz" >/dev/null; then
    break
  fi
  if [ "$(docker inspect -f '{{.State.Running}}' "$name")" != "true" ]; then
    docker logs "$name" >&2
    echo "the edge container stopped before it answered /healthz" >&2
    exit 1
  fi
  sleep 1
done
curl -sf "http://127.0.0.1:${port}/healthz" >/dev/null || {
  docker logs "$name" >&2
  echo "the edge container never answered /healthz" >&2
  exit 1
}
docker stop -t 30 "$name" >/dev/null
code="$(docker inspect -f '{{.State.ExitCode}}' "$name")"
if [ "$code" != "0" ]; then
  docker logs "$name" >&2
  echo "the edge container exited ${code} on SIGTERM, not 0" >&2
  exit 1
fi
echo "edge container image: self-test, /healthz and a clean stop all pass"
