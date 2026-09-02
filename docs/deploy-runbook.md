# Fork-to-UI deploy runbook

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20

How someone forks this repository and reaches a live admin UI, with **no command typed on the
server** ([ADR-0044](adr/0044-fork-and-deploy.md)). The target is ~15 minutes: set a handful of
secrets, run one workflow, enrol yourself. The details of each artifact are in
[`deploy/README.md`](../deploy/README.md); this is the ordered checklist. For the full inventory of
what a fork configures — all 13 repository secrets, and the per-store values that are deliberately
*not* repository secrets — see [`fork-checklist.md`](fork-checklist.md).

> **Fastest test path (no domain).** One Ubuntu VPS with Docker + a public IP is all you need. Set 6
> GitHub Actions secrets — `VPS_HOST`, `VPS_USER`, `VPS_SSH_KEY`, `VPS_KNOWN_HOSTS`, `ACME_EMAIL`, and
> `DOMAIN=<vps-ip-with-dashes>.sslip.io` (e.g. `203-0-113-9.sslip.io`, which resolves to your IP for
> free so you still get real HTTPS). Leave `TLS_MODE` and `CF_DNS_API_TOKEN` unset — unset `TLS_MODE`
> means `acme-http01`, which is exactly what sslip.io wants. Add a `production` Environment
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

> **Four TLS postures, chosen by `TLS_MODE`** ([ADR-0090](adr/0090-tls-postures.md)). Set it
> explicitly; nothing is inferred from the hostname, and an unset value means `acme-http01`.
>
> | `TLS_MODE` | Use it when | Also needs | Caddy image |
> |---|---|---|---|
> | `acme-http01` (default) | sslip.io, or any domain whose A record points at the box and whose `:80` is reachable | `ACME_EMAIL` | stock |
> | `acme-dns01` | a Cloudflare-managed domain, grey-clouded, with no inbound `:80` to ACME | `ACME_EMAIL`, `CF_DNS_API_TOKEN` | custom (plugin build) |
> | `byo-cert` | you already have a certificate — a wildcard, or one from an internal CA — and want no ACME | `secrets/tls/{fullchain,privkey}.pem` on the box | stock |
> | `external` | your own load balancer, ingress, or tunnel already terminates TLS | an upstream terminator that sets `X-Forwarded-*` | stock |
>
> A mode's inputs are **checked and refused**, never quietly downgraded: `acme-dns01` with an empty
> `CF_DNS_API_TOKEN` stops the bootstrap. That used to fall through to HTTP-01, which cannot answer a
> challenge for a DNS-only record — so the cell simply had no certificate, and the log said HTTP-01
> was chosen on purpose.

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
| `TLS_MODE` | the TLS posture: `acme-http01` (the default when unset) · `acme-dns01` · `byo-cert` · `external` ([ADR-0090](adr/0090-tls-postures.md)) |
| `ACME_EMAIL` | contact address for the certificate (required by the two `acme-*` modes) |
| `CF_DNS_API_TOKEN` | the scoped Cloudflare token — **required by `acme-dns01`, unused by every other mode** |
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
- **Certificate export cron (the two `acme-*` modes)**: add the line below so renewals reach
  `secrets/tls/`, which is the path other services read
  ([ADR-0090](adr/0090-tls-postures.md)). `bootstrap.sh` runs the script once on each deploy, so
  the path exists from the first bring-up; cron is what keeps it current across a renewal.

  ```
  17 4 * * * cd ~/pos-cloud/deploy && ./tls-export.sh >> /var/log/tls-export.log 2>&1
  ```

  It exits non-zero and says why when it cannot export. **Nothing alerts on a stale export yet** —
  that follow-up is flagged in ADR-0090 — so until it lands, the log is the only signal, and a
  certificate that stopped being exported fails silently at expiry, weeks later.
- **Changing the TLS posture**: set the `TLS_MODE` secret and re-run the workflow. `TLS_MODE` and
  `DOMAIN` are supplied configuration, not generated secrets, so they are rewritten on the box when
  the environment provides them — unlike the database password, which is minted once and kept.
  Switching to or from `external` also changes `trusted_proxy_hops` in `secrets/cloud.toml`, which
  `bootstrap.sh` reconciles on every run; if it prints a `warn` that it could not (no root, no
  passwordless sudo), fix that line by hand before trusting the `/admin/login` rate limit.
- **Rotating a brought certificate (`byo-cert`)**: replace both files in
  `deploy/secrets/tls/`, then `docker compose -f deploy/compose.yml kill -s HUP caddy`. Nothing
  automates this — the source of those files is outside this deployment by definition.

## 6. The store event bus

Stores publish their committed events to NATS JetStream, and the cloud's rollups, revenue reports,
X/Z aggregation and reconciliation all read what arrives there
([ADR-0089](adr/0089-edge-event-bus-transport.md)). The broker's client port is **published only
when a certificate exists** — `bootstrap.sh` decides that, not a runbook step:

- **certificate in `deploy/secrets/tls/`** → `4222` published on `0.0.0.0`, TLS on.
- **no certificate** → `4222` bound to loopback, TLS off, and the bootstrap log says why.

On the two ACME modes that means a **first deploy leaves the bus closed** — Caddy has not issued yet
when bootstrap runs. Add the `tls-export.sh` cron line above, then redeploy (or wait for cron and
re-run the deploy): the second bootstrap finds the certificate and opens the port. Under
`TLS_MODE=external` nothing on the box issues a certificate, so the bus needs one placed in
`secrets/tls/` before it can open at all.

**Verify reachability before you trust it.** `nats` sits on the `internal: true` Docker network. A
published port is host→container DNAT, which *should* be unaffected by that flag, but this has not
been proven on a real box (ADR-0089 records it as unverified). From a machine outside the VPS:

```
nc -zv <your DOMAIN> 4222
```

If that fails while the bootstrap log says the port is published, the recorded fallback is to add
`- frontend` to the `nats` service's `networks` list in `deploy/compose.yml` and redeploy — at the
cost of giving the broker egress it does not need. Report which one your deployment needed.

**Restrict it at the host firewall** wherever the stores' addresses are knowable. Publishing `4222`
makes the broker internet-facing: no proxy, no Cloudflare, nothing in front of it but its TLS and
its token. Stores on residential or mobile connections have no stable address, so this is a
per-deployment judgement rather than something the compose file can decide.

**Then give each store its URL.** `POS_EDGE_NATS_URL` goes in the store's mode-0600 `env` file (the
new-store wizard emits it as a commented line), and it carries the token from
`deploy/secrets/nats.conf`:

```
POS_EDGE_NATS_URL=tls://:<the token from secrets/nats.conf>@<your DOMAIN>:4222
```

The `tls://` scheme is what makes the client require TLS — `nats://` would connect in plaintext and
the broker would reject it. The token belongs in the userinfo exactly as shown; `link-nats` lifts it
into the connect options, because the client library reads credentials only from there.

Recover the token with `sudo sed -n 's/  token: //p' deploy/secrets/nats.conf`. It is **not**
rotated by a redeploy: `bootstrap.sh` rewrites `nats.conf` on every run but carries the existing
token across, and refuses the run outright if it cannot read it — a rotated token would silently
break every store's publish and the cloud's own cursor at once.

## Kubernetes (optional)

Operators who already run a cluster can use the [`k8s/`](../k8s/README.md) lane instead of Compose. It
mirrors the same four backends and the same secret model; it is not the path the P8 exit criterion
measures, and the Compose lane above stays the default.
