# Fork checklist — everything a fork must configure

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-02
**Relates to** [ADR-0044](adr/0044-fork-and-deploy.md) (what runs on the VPS) · [ADR-0037](adr/0037-api-keys.md) (the scoped store key) · [ADR-0047](adr/0047-minisign-verification.md) (why a signing key exists at all) · [`deploy-runbook.md`](deploy-runbook.md) (the deploy itself) · [`release-runbook.md`](release-runbook.md) (cutting a signed release) · [`guides/bring-a-store-online.md`](guides/bring-a-store-online.md) (per-store provisioning)

This framework is meant to be forked and run by someone else. Until now the settings a fork must
supply were spread across three documents and one of them listed a secret that no workflow reads — so
this page is the single list, and it distinguishes the thing people get wrong most often: **which
secrets belong to GitHub, and which belong to a machine.**

Everything below is derived from the workflows themselves (`grep -rno "secrets\.[A-Z_]*" .github/workflows/`),
not from prose, so it stays checkable.

## 1. GitHub Actions secrets — 12, of which 5 are optional

Repository → Settings → Secrets and variables → Actions.

### Deploy — `.github/workflows/deploy.yml`

| Secret | Required | What |
|---|---|---|
| `VPS_HOST` | yes | host or IP to SSH to |
| `VPS_USER` | yes | the SSH user (a sudo-less deploy user, or root) |
| `VPS_SSH_KEY` | yes | that user's private key, PEM |
| `VPS_KNOWN_HOSTS` | yes | `ssh-keyscan <host>` output (add `-p <port>` for a non-default port), so the deploy is not trust-on-first-use |
| `DOMAIN` | yes | the hostname, or `<vps-ip>.sslip.io` |
| `ACME_EMAIL` | yes | contact address for the certificate |
| `CF_DNS_API_TOKEN` | **conditional** | a Cloudflare token scoped to *Zone → DNS → Edit* on the one zone. Required **unless** `DOMAIN` ends in `.sslip.io`, which issues over HTTP-01 instead |
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

`minisign.pub` is not a secret: record it in the fleet's OTA trust configuration so the edge accepts
signatures from this key, and keep the private half in a password manager or on a hardware key.

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
| `POS_EDGE_NATS_URL` | the same env file | Carries the broker token, and the endpoint differs per deployment |
| `table_token_secret`, the NATS token, `retention_days`, the database password | `deploy/secrets/cloud.toml` on the box | The box mints or holds these; they never leave it. `bootstrap.sh` generates most of them |
| Garage S3 access keys | minted on the box with `garage key create` | Garage generates them at runtime; they cannot be pre-created |
| `RCLONE_REMOTE` | the box's environment, read by `deploy/backup.sh` | **No workflow reads it.** It was previously listed as a GitHub secret; setting it there does nothing and leaves off-box backups silently disabled |

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

## 4. TLS

The default is ACME over HTTP-01, which needs `:80` and `:443` reachable from the internet — an
`*.sslip.io` name satisfies that with no DNS work and no Cloudflare token, which is the fastest way to
a real certificate on a fresh box. Note that such a name is **bound to the IP**: change the VPS
address and the hostname changes with it, and every store's `cloud_url` must be re-issued. Move to a
domain you own before the first real store.

A Cloudflare-managed domain issues over DNS-01 instead and needs `CF_DNS_API_TOKEN`; that path builds
a Caddy image with the DNS provider compiled in, where the sslip.io path uses the stock image.

Bringing your own certificate, or terminating TLS at your own load balancer, is
[D24](roadmap-v3.md) — a `TLS_MODE` setting, landing with ADR-0090.
