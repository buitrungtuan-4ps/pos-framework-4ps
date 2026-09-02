# Fork-to-UI deploy runbook

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20

How someone forks this repository and reaches a live admin UI, with **no command typed on the
server** ([ADR-0044](adr/0044-fork-and-deploy.md)). The target is ~15 minutes: set a handful of
secrets, run one workflow, enrol yourself. The details of each artifact are in
[`deploy/README.md`](../deploy/README.md); this is the ordered checklist. For the full inventory of
what a fork configures — all 12 repository secrets, and the per-store values that are deliberately
*not* repository secrets — see [`fork-checklist.md`](fork-checklist.md).

> **Fastest test path (no domain).** One Ubuntu VPS with Docker + a public IP is all you need. Set 6
> GitHub Actions secrets — `VPS_HOST`, `VPS_USER`, `VPS_SSH_KEY`, `VPS_KNOWN_HOSTS`, `ACME_EMAIL`, and
> `DOMAIN=<vps-ip-with-dashes>.sslip.io` (e.g. `203-0-113-9.sslip.io`, which resolves to your IP for
> free so you still get real HTTPS). Leave `CF_DNS_API_TOKEN` unset. Add a `production` Environment
> with yourself as required reviewer. Then **Actions → deploy → Run workflow**. Everything else below
> is the with-a-domain and day-two detail. This deploys the **cloud** tier only; the store tier
> (`pos_edge`) runs in the shop — see [Start from zero](guides/start-from-zero.md).

## 0. What you need first

- A **VPS** (~1.5 GB RAM is enough for the four backends) reachable over SSH, with Docker and the
  Compose plugin installed.
- Either a **domain** you control through Cloudflare, or nothing — in which case you will use the
  `<vps-ip>.sslip.io` fallback and skip the Cloudflare steps.
- A place for **off-box backups** (any rclone remote): object storage, another host, a personal
  drive. Optional for a first bring-up, required before you trust the cell with real data
  ([ADR-0046](adr/0046-backups-and-restore.md)).

> **Two TLS modes, chosen automatically by `DOMAIN`.** A `*.sslip.io` DOMAIN issues over
> **HTTP-01/TLS-ALPN** with the **stock official Caddy image** — no Cloudflare, no token, and the
> custom plugin build is skipped entirely. A managed domain issues over **DNS-01 via Cloudflare**,
> which needs the custom Caddy image (built with the `caddy-dns/cloudflare` plugin) and a
> `CF_DNS_API_TOKEN`. The deploy workflow and `bootstrap.sh` pick the right image and Caddyfile from
> `DOMAIN`; you set no flag.

## 1. Cloudflare (skip if using sslip.io)

The cell's own host follows the same rule as tenant hostnames — **redirect, never proxy, and never
Flexible** ([ADR-0023](adr/0023-tenant-hostname-and-slug.md)):

1. Create an **A record** for your host pointing at the VPS IP, and leave it **grey-clouded**
   (DNS-only). Caddy issues the certificate over DNS-01, so nothing inbound on `:80` has to reach
   ACME and the origin is never proxied.
2. If you ever orange-cloud a record, its SSL mode **must be Full (strict)** — Cloudflare's
   "Flexible" mode serves the browser HTTPS while fetching your origin over plaintext, a downgrade
   the padlock hides. It is forbidden here; Caddy always terminates real TLS.
3. Create a **scoped API token**: `Zone → DNS → Edit` on that one zone, nothing wider. This is
   `CF_DNS_API_TOKEN`.

## 2. GitHub secrets and the production Environment

Set these repository **secrets** — the only values that ever leave the repo boundary, and none is an
application secret (the box mints those itself, [ADR-0044](adr/0044-fork-and-deploy.md)):

| Secret | What |
|---|---|
| `VPS_HOST` | host or IP to SSH to |
| `VPS_USER` | SSH user (a sudo-less deploy user, or root) |
| `VPS_SSH_KEY` | that user's private key (PEM) |
| `VPS_KNOWN_HOSTS` | the box's SSH host key(s) — `ssh-keyscan <host>` (add `-p <port>` for a non-default port) — so the deploy is not trust-on-first-use |
| `DOMAIN` | your host (or `<vps-ip>.sslip.io`) |
| `ACME_EMAIL` | contact address for the certificate |
| `CF_DNS_API_TOKEN` | the scoped Cloudflare token (leave empty for sslip.io) |
| `VPS_PORT` | SSH port, if not 22 (optional; defaults to 22 when unset or empty) |

> `RCLONE_REMOTE` used to be listed here as a repository secret. It is **not** one — no workflow reads
> it. It is an environment variable on the box, read by [`deploy/backup.sh`](../deploy/backup.sh) when
> it ships a dump off-box. Setting it as a GitHub secret does nothing and leaves the off-box backup
> tier silently disabled, which is the worst place for a silent no-op. See
> [`fork-checklist.md`](fork-checklist.md) for the full list of what is and is not a repository secret.

Then configure a GitHub **Environment named `production`** with a **required reviewer**. Every deploy
runs in it, so the reviewer is the second human the admin break-glass needs
([ADR-0045](adr/0045-first-boot-admin-enrolment.md)); without the reviewer, `reset_admin` is a
single-actor action.

## 3. Run the deploy

Actions → **deploy** → *Run workflow* (leave `reset_admin` off). It builds the images, ships them
over SSH, and runs [`bootstrap.sh`](../deploy/bootstrap.sh) on the box, which generates every internal
secret locally and brings the stack up. The run's log prints a **one-time super-admin setup token**
once — copy it.

## 4. Enrol yourself

Open `https://<your DOMAIN>/` — Caddy has a real certificate by now. Enrol the first super-admin:

```
curl -X POST https://<your DOMAIN>/admin/setup \
  -H 'content-type: application/json' \
  -d '{"setup_token":"<the token from the run log>","password":"<a strong passphrase, 12+ chars>"}'
```

The response carries an `otpauth://` URI once — add it to your authenticator app (or scan it as a
QR). That is your TOTP second factor. The route now refuses further enrolments (`409`), so the token
is spent. Sign in at `/admin/login` with the password and a current code.

## 5. Day-two

- **Redeploy / upgrade**: bump the images in `deploy/compose.yml` (or land new code) and re-run the
  workflow. It snapshots the database first (`.pre-update`) and keeps every existing secret.
- **Rollback**: re-run the workflow from an older commit — its image tag names a specific build, and
  the app container is stateless.
- **Lost your authenticator**: re-run the workflow with **`reset_admin=true`** (the reviewer must
  approve). It clears the super-admin and all sessions; enrol again from step 4 with the token now in
  `secrets/cloud.toml` on the box.
- **Backups**: set `RCLONE_REMOTE` so the daily dump and WAL ship off-box, and let the nightly
  `restore-drill` prove they restore ([ADR-0046](adr/0046-backups-and-restore.md)).

## Kubernetes (optional)

Operators who already run a cluster can use the [`k8s/`](../k8s/README.md) lane instead of Compose. It
mirrors the same four backends and the same secret model; it is not the path the P8 exit criterion
measures, and the Compose lane above stays the default.
