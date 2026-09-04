# Fork checklist — everything a fork must configure

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-02
**Relates to** [ADR-0044](adr/0044-fork-and-deploy.md) (what runs on the VPS) · [ADR-0037](adr/0037-api-keys.md) (the scoped store key) · [ADR-0047](adr/0047-minisign-verification.md) (why a signing key exists at all) · [`deploy-runbook.md`](deploy-runbook.md) (the deploy itself) · [`release-runbook.md`](release-runbook.md) (cutting a signed release) · [`guides/bring-a-store-online.md`](guides/bring-a-store-online.md) (per-store provisioning)

This framework is meant to be forked and run by someone else. Until now the settings a fork must
supply were spread across three documents and one of them listed a secret that no workflow reads — so
this page is the single list, and it distinguishes the thing people get wrong most often: **which
secrets belong to GitHub, and which belong to a machine.**

Everything below is derived from the workflows themselves (`grep -rno "secrets\.[A-Z_]*" .github/workflows/`),
not from prose, so it stays checkable.

## 1. GitHub Actions secrets — 13, of which 6 are optional

Repository → Settings → Secrets and variables → Actions.

### Deploy — `.github/workflows/deploy.yml`

| Secret | Required | What |
|---|---|---|
| `VPS_HOST` | yes | host or IP to SSH to |
| `VPS_USER` | yes | the SSH user (a sudo-less deploy user, or root) |
| `VPS_SSH_KEY` | yes | that user's private key, PEM |
| `VPS_KNOWN_HOSTS` | yes | `ssh-keyscan <host>` output (add `-p <port>` for a non-default port), so the deploy is not trust-on-first-use |
| `DOMAIN` | yes | the hostname, or `<vps-ip>.sslip.io` |
| `TLS_MODE` | no | the TLS posture ([ADR-0090](adr/0090-tls-postures.md)): `acme-http01` · `acme-dns01` · `byo-cert` · `external`. Unset means `acme-http01`, which is what sslip.io and any A-record hostname want. It also decides which Caddy image CI builds |
| `ACME_EMAIL` | **conditional** | contact address for the certificate. Required by the two `acme-*` modes; unused by `byo-cert` and `external` |
| `CF_DNS_API_TOKEN` | **conditional** | a Cloudflare token scoped to *Zone → DNS → Edit* on the one zone. Required by **`TLS_MODE=acme-dns01` and nothing else**. That mode refuses to bootstrap without it rather than falling back to HTTP-01, which cannot answer a challenge for a DNS-only record |
| `VPS_PORT` | no | SSH port when it is not 22 |

### Release — `.github/workflows/release.yml`

| Secret | Required | What |
|---|---|---|
| `MINISIGN_SECRET_KEY` | yes | the whole of `minisign.key`, both lines. The workflow **fails before it builds** without it, because a release is never published unsigned |
| `MINISIGN_PASSWORD` | no | omit for a passwordless CI key (`minisign -G -W`) |

Put both on the **`production` Environment**, not at repository level, so cutting a release inherits
the same required-reviewer protection as a deploy.

Generate the key **offline, on your own machine** — never on a runner or the VPS ([D1](roadmap-v3.md)):

```
minisign -G -W -p minisign.pub -s minisign.key
```

`minisign.pub` is not a secret. Keep the private half in a password manager or on a hardware key,
and give the **public** half to the build as a repository **variable** (not a secret — a variable is
visible in the run log, where an operator can see which anchor a release was built against):

| Variable | Required | What |
|---|---|---|
| `POS_EDGE_TRUSTED_KEYS` | yes | the **second line** of `minisign.pub`, verbatim. Comma-separate two of them to keep a retirement path open. `release.yml` **fails before it builds** without it |

```
POS_EDGE_TRUSTED_KEYS="$(sed -n 2p minisign.pub)"
```

**It is a build input, and only a build input** ([ADR-0092](adr/0092-artifact-trust-chain.md)). An
earlier version of this page said to "record it in the fleet's OTA trust configuration", which reads
like the cloud-published config tree — and a key taken from there is a key an attacker who controls
the cloud can choose, which makes the signature check verify *their* artifact against *their* key. A
trust anchor cannot live inside the channel it protects, so `crates/pos-edge/src/trusted_keys.rs`
reads it through `option_env!` and `pos-edge` exposes no way to supply one at runtime.

Keep **two** keys baked in where you can ([ADR-0047](adr/0047-minisign-verification.md)): retiring a
compromised key otherwise needs a release that the compromised key itself must sign.

### Mirror — `.github/workflows/mirror.yml`

| Secret | Required | What |
|---|---|---|
| `MIRROR_REMOTE` | no | a second git remote to mirror to. Unset, the job logs a notice and skips |
| `MIRROR_SSH_KEY` | no | that remote's deploy key |

### Also required, and not a secret

- A GitHub **Environment named `production`** with a **required reviewer**. Both `deploy.yml` and
  `release.yml` declare `environment: production`, and that reviewer is the second human the admin
  break-glass depends on ([ADR-0045](adr/0045-first-boot-admin-enrolment.md)) — without one,
  `reset_admin` is a single-actor action.
- `GITHUB_TOKEN` is provided automatically. Nothing to do.

## 2. NOT GitHub secrets — the mistake worth avoiding

These are **per-machine** or **per-store** values. A repository-level secret is the wrong home for
every one of them, and in a fork it would travel with the repository.

| Value | Where it actually lives | Why not GitHub |
|---|---|---|
| `POS_EDGE_SYNC_KEY` | the store box: OS keyring (`SecretName::SyncKey`), or `/etc/pos-edge/env` mode-0600 root-owned | It is one key **per store**. A hundred stores means a hundred different values; a repository secret is a single value |
| `POS_EDGE_NATS_URL` | the same env file | Carries the broker token, and the endpoint differs per deployment. Shape: `tls://:<token>@<your DOMAIN>:4222` — the `tls://` scheme is what makes the client require TLS, and the token comes from `deploy/secrets/nats.conf` on the box ([ADR-0089](adr/0089-edge-event-bus-transport.md)) |
| `table_token_secret`, `internal_shared_secret`, `alert_webhook_secret`, the NATS token, `retention_days`, the database password | `deploy/secrets/cloud.toml` on the box | The box mints or holds these; they never leave it. `bootstrap.sh` generates most of them. **`internal_shared_secret` is required** — `pos_cloud` refuses to start without it (ADR-0097), so an upgraded box needs the line added before it will boot. `alert_webhook_secret` is optional and pairs with `alert_webhook_url`: set **both** to push alerts off-console, or neither to stay console-only — half the pair is a boot refusal, because a URL without a secret delivers batches a receiver cannot tell from forgeries ([ADR-0073](adr/0073-alerting.md)) |
| Garage S3 access keys | minted on the box by `bootstrap.sh`, into `cloud.toml`'s `[artifacts]` | Garage generates them at runtime, so they cannot be pre-created — but the capture is scripted (ADR-0088), not a step anyone performs |
| `RCLONE_REMOTE` | the box's environment, read by `deploy/backup.sh` | **No workflow reads it.** It was previously listed as a GitHub secret; setting it there does nothing and leaves off-box backups silently disabled |
| The TLS certificate and key under `TLS_MODE=byo-cert` | `deploy/secrets/tls/{fullchain,privkey}.pem` on the box, root-owned, the key mode 0600 | A private key must not cross the repository boundary, and `TLS_MODE` is the only part of that posture GitHub needs to know ([ADR-0090](adr/0090-tls-postures.md)) |
| `EXTERNAL_HTTP_PORT` | the box's environment on the first bootstrap under `TLS_MODE=external`; recorded in the generated `deploy/.env` | A port binding on one machine. Default `80`; set it when the box already runs something there |

## 3. Per-store provisioning

The console's guided new-store wizard produces both files an operator carries to a box:

- **`config.toml`** — `store_id` and `cloud_url`, plus an optional `bind` port. No secret. Without
  `cloud_url` the box boots LAN-only and its activation screen 404s, which is why the wizard now
  always emits it.
- **`env`** — `POS_EDGE_SYNC_KEY`, and `POS_EDGE_NATS_URL` when the event bus is reachable. Install it
  as root, mode 0600, at `/etc/pos-edge/env`.

**The store key needs both `read_config` and `relay_orders`.** With only `read_config` a store looks
healthy — configuration syncs, the dashboard shows it alive — while the order relay answers `403` on
every poll, so orders placed in the cloud never reach the kitchen. The wizard pre-selects both.

**Two scopes are dead.** `read_events` and `manage_webhooks` exist in the cloud's `Scope` enum but
gate no route, so they are deliberately absent from both scope pickers. Do not issue a key expecting
them to grant anything.

## 4. TLS — pick one of four postures

`TLS_MODE` is explicit; nothing is inferred from the hostname
([ADR-0090](adr/0090-tls-postures.md)). Unset means `acme-http01`.

| `TLS_MODE` | Pick it when | Also needs | Caddy image |
|---|---|---|---|
| `acme-http01` (default) | sslip.io, or any domain whose A record points at the box and whose `:80` is reachable from the internet | `ACME_EMAIL` | stock |
| `acme-dns01` | a Cloudflare-managed domain, grey-clouded, with no inbound `:80` reaching ACME | `ACME_EMAIL`, `CF_DNS_API_TOKEN` | custom, with the DNS plugin compiled in |
| `byo-cert` | you already hold a certificate — a wildcard, or one from your internal CA — and want no ACME anywhere near this box | `deploy/secrets/tls/{fullchain,privkey}.pem` installed on the box | stock |
| `external` | your own load balancer, ingress controller, or tunnel already terminates TLS | an upstream terminator that sets `X-Forwarded-*` | stock |

The fastest path on a fresh box is the default with an `*.sslip.io` name: no DNS work, no Cloudflare
token, and a real certificate. But such a name is **bound to the IP** — change the VPS address and
the hostname changes with it, and every store's `cloud_url` must be re-issued. Move to a domain you
own before the first real store.

**`byo-cert`.** Put `fullchain.pem` and `privkey.pem` in `deploy/secrets/tls/` on the box before
deploying. `bootstrap.sh` refuses to bring the stack up when either is missing, rather than starting
a Caddy that fails to load them. Renewal is yours: replace both files and
`docker compose -f deploy/compose.yml kill -s HUP caddy`. Nothing here can renew a certificate it did
not issue.

**`external`.** Two things change beyond the proxy. `443` stops being offered to the internet (it is
published on loopback, because Compose cannot conditionally omit a port), and the site is served as
plain HTTP on `EXTERNAL_HTTP_PORT` (default `80`) for your terminator to reach — firewall it to that
terminator. And `trusted_proxy_hops` becomes `2`, which `bootstrap.sh` writes into
`secrets/cloud.toml` for you. **If that value is wrong, the `/admin/login` rate limit keys every
request on your balancer's single address**: all admins share one bucket and one person's wrong
passwords lock out the rest. Watch the bootstrap log for a `warn` that it could not write the line.

**The certificate path.** `deploy/secrets/tls/` is where every consumer looks, in all four modes. On
the two ACME modes, `deploy/tls-export.sh` republishes Caddy's certificate there; add its cron line
from the [deploy runbook](deploy-runbook.md) so a renewal reaches the exported copy. Nothing alerts
on a stale export yet — a flagged follow-up in ADR-0090 — so an exporter that quietly stops shows up
only when the old certificate expires.

## 5. The store event bus

Stores publish their events to NATS, and everything the cloud reports is downstream of that
([ADR-0089](adr/0089-edge-event-bus-transport.md)). Two things a fork needs to know:

- **The port opens only with a certificate.** `bootstrap.sh` publishes `4222` on `0.0.0.0` with TLS
  when `deploy/secrets/tls/` holds one, and binds it to loopback otherwise. On the ACME modes a
  **first deploy leaves it closed** — Caddy has not issued yet — so add the `tls-export.sh` cron line
  and redeploy. Under `TLS_MODE=external` nothing here issues a certificate, so one has to be
  brought before the bus can open.
- **Publishing it makes the broker internet-facing.** No proxy, no Cloudflare: its TLS and its token
  are the only things in front of the fleet's event stream. Restrict `4222` at the host firewall
  wherever the stores' addresses are knowable, and verify reachability from outside before trusting
  it — the interaction between Docker's `internal` network flag and a published port is recorded in
  ADR-0089 as **unverified**, with a one-line fallback in the runbook.

The token is never rotated by a redeploy, and a bootstrap that cannot read the existing one refuses
rather than minting a replacement — rotating it would break every store's publish and the cloud's
ingest cursor in the same instant.
