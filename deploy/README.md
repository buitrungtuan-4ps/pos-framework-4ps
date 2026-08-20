# Deploying a `pos_cloud` country cell

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20

One country cell runs on **one VPS** as a Docker Compose stack ([ADR-0044](../docs/adr/0044-fork-and-deploy.md)).
This directory holds the deployment artifacts; the store-side service unit lives under
[`edge/`](edge/). The end-to-end fork-to-UI runbook is P8e; this README documents the stack
itself so it is reviewable and operable on its own.

## What runs

Five containers, defined in [`compose.yml`](compose.yml):

| Service | Image | Role |
|---|---|---|
| `caddy` | built from [`caddy.Dockerfile`](caddy.Dockerfile) | TLS ingress; terminates HTTPS and reverse-proxies to `pos_cloud` |
| `pos_cloud` | built from [`Dockerfile`](Dockerfile) | the cloud binary — ingest → rollups, `/v1` API, `/admin`, webhooks |
| `postgres` | `postgres:16.4-bookworm` | the event log, rollups, config tree, subjects ([ADR-0016](../docs/adr/0016-postgres-access.md)) |
| `nats` | `nats:2.10.20-alpine` | JetStream ingest feed ([ADR-0031](../docs/adr/0031-cloud-adapter-transports.md)) |
| `garage` | `dxflrs/garage:v1.0.1` | object storage for menu images ([ADR-0031](../docs/adr/0031-cloud-adapter-transports.md)) |

Only `caddy` faces the internet (ports 80/443); the four backends sit on an `internal`
Docker network with no egress except `pos_cloud`'s webhook deliveries. Each container caps
its log size and its memory/CPU/pids so one runaway service cannot starve the box — the four
backends fit ~1.4 GB, sized for a small VPS.

## Secrets — generated on the box, never committed

`compose.yml` reads four files under `secrets/`, all git-ignored and **generated on the
server** by `bootstrap.sh` (P8b), never checked in and never returned to GitHub:

| File | Holds |
|---|---|
| `secrets/pos.env` | `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` |
| `secrets/cloud.toml` | `pos_cloud` config: `bind = "0.0.0.0:8080"`, `database_url` (with the postgres password, `host = "postgres"`), optional `[nats]` |
| `secrets/garage.toml` | Garage server config (`rpc_secret`, data/meta paths) |
| `secrets/caddy.env` | `DOMAIN` / `ACME_EMAIL` / `CF_DNS_API_TOKEN` for TLS issuance |

`secrets/cloud.toml` **must be `chmod 600` and `chown 10001:10001`** — the `pos_cloud`
container runs as the non-root uid `10001`, and a `600` file readable only by root would be
invisible to it. `bootstrap.sh` sets both.

GitHub holds only the handful of values needed to *reach* the box and issue a certificate
(`VPS_HOST`, `DOMAIN`, `ACME_EMAIL`, `CF_DNS_API_TOKEN`, the off-box backup credentials) —
never an application secret. A leaked deploy token exposes the channel, not the database.

## TLS

Caddy obtains and renews the certificate over **DNS-01** by default (a scoped
`CF_DNS_API_TOKEN`), so the box needs no inbound `:80` reachable to ACME and the DNS record
can stay grey-clouded. With no purchased domain, set `DOMAIN=<vps-ip>.sslip.io` and switch
[`Caddyfile`](Caddyfile) to the documented HTTP-01 fallback. Cloudflare **"Flexible" SSL is
forbidden** — the origin is always real TLS.

## Bring-up

Once `bootstrap.sh` (P8b) has written `secrets/*`:

```
docker compose up -d --build
```

A rollback is re-running with an older `POS_CLOUD_IMAGE` tag — the app container is
stateless; all durable state lives in the `postgres` / `garage` / `nats` volumes. The deploy
workflow (P8c) ships a pinned image over SSH and runs this; backups and the weekly restore
drill (P8d) are the durability half.

> This environment has no Docker daemon, so these artifacts are validated by YAML/`bash -n`
> parsing, the `actions-pinned` gate, and review. The true end-to-end check is the P8 exit:
> a human forks, sets the secrets, runs the workflow, and reaches the admin UI.
