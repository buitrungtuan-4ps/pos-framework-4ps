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
| `caddy` | the stock `caddy:2.8.4`, or built from [`caddy.Dockerfile`](caddy.Dockerfile) on `TLS_MODE=acme-dns01` | TLS ingress; terminates HTTPS and reverse-proxies to `pos_cloud` |
| `pos_cloud` | built from [`Dockerfile`](Dockerfile) | the cloud binary — ingest → rollups, `/v1` API, `/admin`, webhooks |
| `postgres` | `postgres:16.4-bookworm` | the event log, rollups, config tree, subjects ([ADR-0016](../docs/adr/0016-postgres-access.md)) |
| `nats` | `nats:2.10.20-alpine` | JetStream ingest feed ([ADR-0031](../docs/adr/0031-cloud-adapter-transports.md)) |
| `garage` | `dxflrs/garage:v1.0.1` | object storage for menu images ([ADR-0031](../docs/adr/0031-cloud-adapter-transports.md)) |

Only `caddy` faces the internet (ports 80/443 — HTTP only, with `443` bound to loopback, under
`TLS_MODE=external`); the four backends sit on an `internal`
Docker network with no egress except `pos_cloud`'s webhook deliveries. Each container caps
its log size and its memory/CPU/pids so one runaway service cannot starve the box — the four
backends fit ~1.4 GB, sized for a small VPS.

## Secrets — generated on the box, never committed

`compose.yml` reads these files under `secrets/`, all git-ignored and **generated on the
server** by `bootstrap.sh` (P8b), never checked in and never returned to GitHub:

| File | Holds |
|---|---|
| `secrets/pos.env` | `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` |
| `secrets/cloud.toml` | `pos_cloud` config: `bind = "0.0.0.0:8080"`, `database_url` (with the postgres password, `host = "postgres"`), the one-time `admin_setup_token` (ADR-0045), optional `[nats]` |
| `secrets/nats.conf` | NATS JetStream + a token enforced on the internal network |
| `secrets/garage.toml` | Garage server config (`rpc_secret`, data/meta paths) |
| `secrets/caddy.env` | `TLS_MODE` / `DOMAIN` / `ACME_EMAIL` / `CF_DNS_API_TOKEN` / `TLS_RELOAD_SERVICES` — the TLS posture and hostname ([ADR-0090](../docs/adr/0090-tls-postures.md)) |
| `secrets/Caddyfile` | the generated Caddyfile for that posture, copied from [`Caddyfile.d/`](Caddyfile.d) |
| `secrets/tls/` | `fullchain.pem` + `privkey.pem` — the one certificate path every consumer reads |

`secrets/cloud.toml` **must be `chmod 600` and `chown 10001:10001`** — the `pos_cloud`
container runs as the non-root uid `10001`, and a `600` file readable only by root would be
invisible to it. `bootstrap.sh` sets both.

GitHub holds only the handful of values needed to *reach* the box and choose a TLS posture
(`VPS_HOST`, `DOMAIN`, `TLS_MODE`, `ACME_EMAIL`, `CF_DNS_API_TOKEN`) — never an application
secret. A leaked deploy token exposes the channel, not the database. The full inventory, and what
is deliberately *not* a repository secret, is in [`fork-checklist.md`](../docs/fork-checklist.md).

`bootstrap.sh` also writes a generated `.env` beside `compose.yml` — not a secret, just the port
publishes and image tags the posture implies, so a later `docker compose up -d` typed by hand
reproduces the same posture instead of reverting to the defaults.

## TLS — four postures, chosen by `TLS_MODE`

`TLS_MODE` is explicit and nothing is inferred from the hostname
([ADR-0090](../docs/adr/0090-tls-postures.md)). Unset means `acme-http01`.

| `TLS_MODE` | Certificate from | Caddy image | Published | `secrets/tls/` filled by | `trusted_proxy_hops` |
|---|---|---|---|---|---|
| `acme-http01` (default) | ACME HTTP-01 / TLS-ALPN | stock | `80`, `443`, `443/udp` | [`tls-export.sh`](tls-export.sh) | 1 |
| `acme-dns01` | ACME DNS-01 via Cloudflare | [`caddy.Dockerfile`](caddy.Dockerfile) | `80`, `443`, `443/udp` | [`tls-export.sh`](tls-export.sh) | 1 |
| `byo-cert` | the files the operator installed | stock | `80`, `443`, `443/udp` | the operator | 1 |
| `external` | nothing on this box | stock | HTTP only; `443` on loopback | nobody | **2** |

Each mode is a committed file under [`Caddyfile.d/`](Caddyfile.d) that imports one shared
`site.caddy`; `bootstrap.sh` installs the selected one as `secrets/Caddyfile`. Nothing overwrites
a version-controlled file, and the posture is recorded in `secrets/caddy.env` so the box can be
asked which one it is in.

Two rules hold in every mode. Cloudflare **"Flexible" SSL is forbidden** — the origin is always
real TLS ([ADR-0023](../docs/adr/0023-tenant-hostname-and-slug.md)). And a mode's inputs are
**checked and refused**, never downgraded: `acme-dns01` without `CF_DNS_API_TOKEN` stops the
bootstrap rather than falling back to a challenge that cannot answer for a DNS-only record.

`secrets/tls/` is the single certificate path. On the two ACME modes,
[`tls-export.sh`](tls-export.sh) republishes Caddy's certificate there — reading it *through* the
container, refusing to guess when the ACME-directory glob is not unique, writing only on a real
change, and `SIGHUP`-ing whatever `TLS_RELOAD_SERVICES` names. Run it from cron; the line is in
the [deploy runbook](../docs/deploy-runbook.md).

## Bootstrap and bring-up

[`bootstrap.sh`](bootstrap.sh) generates `secrets/*` on the box and brings the stack up. It
is idempotent — an existing *generated* secret is kept, never rotated — so it is safe to re-run.
Only the reach-the-box and TLS values come from outside, in the environment:

```
DOMAIN=cloud.example.com TLS_MODE=acme-dns01 ACME_EMAIL=ops@example.com \
  CF_DNS_API_TOKEN=xxxx ./bootstrap.sh
```

With no purchased domain, the default posture and the sslip.io hostname are enough:

```
DOMAIN=203-0-113-9.sslip.io ACME_EMAIL=ops@example.com ./bootstrap.sh
```

`TLS_MODE` and `DOMAIN` are *supplied configuration*, not generated secrets, so a re-run **with
them in the environment rewrites `secrets/caddy.env`** — changing the posture or the hostname and
redeploying takes effect. A re-run with no environment keeps what the box already has.

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
`VPS_SSH_KEY`, `VPS_KNOWN_HOSTS`, `DOMAIN`, `TLS_MODE`, `ACME_EMAIL`, `CF_DNS_API_TOKEN`) are the
only values that cross the repo boundary; none is an application secret. `TLS_MODE` also decides
which Caddy image CI builds — the plugin build happens only for `acme-dns01`.

Running it with `reset_admin=true` invokes [`reset-admin.sh`](reset-admin.sh) after bring-up,
which clears the super-admin and every session (`DELETE FROM super_admin; DELETE FROM
admin_sessions;`) so a locked-out operator can re-enrol at `/admin/setup`. Reset lives here,
not in the app, so no reset flag ever rides in the container's environment.

A rollback is re-running the workflow at an older commit — its image tag names a specific
build, and the app container is stateless; all durable state lives in the `postgres` /
`garage` / `nats` volumes. Backups and the weekly restore drill (P8d) are the durability half.

## Backups and restore

Durability has four unequal classes ([ADR-0046](../docs/adr/0046-backups-and-restore.md)), because
not all data is worth the same recovery point:

1. **Continuous WAL** — Postgres archives each filled segment to the `wal_archive` volume
   (`archive_mode=on` in `compose.yml`); the box cron ships the segments off-box with `rclone`.
   Recovery point is minutes, not a day.
2. **Daily cloud-database dump** — [`backup.sh`](backup.sh) streams a compressed `pg_dump` and, if
   `RCLONE_REMOTE` is set, copies it to the off-box tier. A backup on the database's own box is not a
   backup.
3. **Garage object sync** — menu images, weekly, off-box; lowest value because they regenerate from
   the tenant's source uploads.
4. **The `.pre-update` snapshot** — the deploy workflow runs `backup.sh --label pre-update` before a
   new image comes up, so a bad release rolls straight back to a known-good database.

[`restore-drill.sh`](restore-drill.sh) is the proof the backups are real: it dumps the live database,
restores it into a throwaway one, and reconciles every table's row count against the source — a
silently unrestorable backup fails it. It runs for real each night against a service Postgres in
[`nightly.yml`](../.github/workflows/nightly.yml)'s `restore-drill` job, and can be pointed at the box
(a reachable `PGHOST`) for the weekly drill on production data. The store-backup half of the drill is
edge WAL shipping (P9, spike A4) and joins when that lands.

> This environment has no Docker daemon, so these artifacts are validated by YAML/`bash -n`
> parsing, the `actions-pinned` gate, and review (the nightly `restore-drill` job exercises the
> restore path for real on its own schedule). The true end-to-end check is the P8 exit: a human
> forks, sets the secrets, runs the workflow, and reaches the admin UI.
