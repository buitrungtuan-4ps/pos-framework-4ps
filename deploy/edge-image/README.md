# The edge container image

One store's `pos_edge` in a container, for a store whose edge is hosted rather than in the shop
([ADR-0110](../../docs/adr/0110-edge-placement-is-a-deployment-axis.md),
[ADR-0113](../../docs/adr/0113-the-host-agent.md)). Roadmap-v3 Program C.

| File | What it is |
|---|---|
| [`Dockerfile`](Dockerfile) | Wraps a **released** `pos_edge` binary (copied in beside it), on Debian 13 slim, as an unprivileged uid, with the store's state in one volume at `/var/lib/pos-edge`. It compiles nothing, so the container runs the bytes a store PC runs |
| [`compose.yml`](compose.yml) | One store, as a host runs it: its own uid, its own volume, its port on `127.0.0.1` only, a read-only root, no capabilities, `restart: on-failure` so exit `10` (restart wanted) and `11` (stopped undrained) start it again and `0` does not |
| [`smoke.sh`](smoke.sh) | Builds the image around a binary, runs `--self-test` against a fake store's config, starts it, waits for `/healthz`, stops it with `SIGTERM` and checks it exits `0`. The `edge-image` workflow runs it on every change here and after every merge |

## Running one store by hand

```bash
cp target/x86_64-unknown-linux-gnu/release/pos-edge deploy/edge-image/pos-edge
cd deploy/edge-image
STORE=store-a POS_EDGE_PORT=18787 POS_EDGE_UID=10002 docker compose -p store-a run --rm edge claim
STORE=store-a POS_EDGE_PORT=18787 POS_EDGE_UID=10002 docker compose -p store-a up -d
```

`claim` shows a code, waits for the console to bind it to a store, and writes the store's
`config.toml` into the volume ([ADR-0148](../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)).
Put the host's reverse proxy in front of the loopback port, one hostname per store (ADR-0113).

## What is not here yet

- **The host agent** (`pos_host`, ADR-0113): the dial-out process that pulls region-scoped jobs from
  the cloud, starts one container per store with a single-use activation code, swaps a store's
  image when its ring says so, and reports capacity. Not built: it is a third binary with its own
  cloud routes, and until it exists a person runs the compose file above.
- **Updates.** The edge in a container never installs its own release (it finds no `bin/current`
  and starts no updater); a new release is a new image over the same volume.
- **A published image.** CI builds and smoke-tests the image; publishing it to a registry is the
  release owner's, as signing is.
