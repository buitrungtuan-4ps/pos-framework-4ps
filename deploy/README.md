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
| `secrets/cloud.toml` | `pos_cloud` config: `bind = "0.0.0.0:8080"`, `database_url` (with the postgres password, `host = "postgres"`), the one-time `admin_setup_token` (ADR-0045), optional `[nats]` |
| `secrets/nats.conf` | NATS JetStream + a token enforced on the internal network |
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

## Bootstrap and bring-up

[`bootstrap.sh`](bootstrap.sh) generates `secrets/*` on the box and brings the stack up. It
is idempotent — an existing secret is kept, never rotated — so it is safe to re-run. Only the
TLS values come from outside the box, passed in the environment on the first run:

```
DOMAIN=cloud.example.com ACME_EMAIL=ops@example.com CF_DNS_API_TOKEN=xxxx ./bootstrap.sh
```

With no purchased domain, use the sslip.io fallback (HTTP-01, no Cloudflare token):

```
DOMAIN=203-0-113-9.sslip.io ACME_EMAIL=ops@example.com ./bootstrap.sh
```

It prints a **one-time super-admin setup token** once (also written as `admin_setup_token` in
`secrets/cloud.toml`). Enrol the first super-admin with it at `POST /admin/setup` — the route
takes a chosen password and returns the TOTP enrolment once, then 409s ([ADR-0045](../docs/adr/0045-first-boot-admin-enrolment.md)).
Set `POS_BOOTSTRAP_NO_UP=1` to generate secrets without starting the stack, or
`POS_BOOTSTRAP_NO_BUILD=1` (with `POS_CLOUD_IMAGE`/`CADDY_IMAGE`) to run prebuilt images
without rebuilding — the mode the deploy workflow uses.

## Deploy workflow and the reset break-glass

[`.github/workflows/deploy.yml`](../.github/workflows/deploy.yml) (manual `workflow_dispatch`)
builds both images in CI, ships them over the existing SSH channel (no registry), and runs
`bootstrap.sh` on the box. It runs in the `production` GitHub Environment — **configure that
Environment with a required reviewer**, so every deploy, and the break-glass in particular,
is approved by a second human. The 4–6 GitHub secrets it needs (`VPS_HOST`, `VPS_USER`,
`VPS_SSH_KEY`, `VPS_KNOWN_HOSTS`, `DOMAIN`, `ACME_EMAIL`, `CF_DNS_API_TOKEN`) are the only
values that cross the repo boundary; none is an application secret.

Running it with `reset_admin=true` invokes [`reset-admin.sh`](reset-admin.sh) after bring-up,
which clears the super-admin and every session (`DELETE FROM super_admin; DELETE FROM
admin_sessions;`) so a locked-out operator can re-enrol at `/admin/setup`. Reset lives here,
not in the app, so no reset flag ever rides in the container's environment.

A rollback is re-running the workflow at an older commit — its image tag names a specific
build, and the app container is stateless; all durable state lives in the `postgres` /
`garage` / `nats` volumes. Backups and the weekly restore drill (P8d) are the durability half.

> This environment has no Docker daemon, so these artifacts are validated by YAML/`bash -n`
> parsing, the `actions-pinned` gate, and review. The true end-to-end check is the P8 exit:
> a human forks, sets the secrets, runs the workflow, and reaches the admin UI.
