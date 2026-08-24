# Start from zero

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21

From a fresh clone to (1) a sale happening on your laptop, and (2) the cloud running on a real server.
Two parts, because there are [two tiers](README.md#the-one-thing-to-understand-first-there-are-two-tiers).

## Prerequisites

- **Rust** — install [rustup](https://rustup.rs). The exact version is pinned in `rust-toolchain.toml`,
  so `rustup` picks it up automatically; you do not choose a version.
- **`just`** (optional but recommended): `cargo install just`. Every command below is also shown as the
  raw `cargo` line, so you can skip `just` entirely.
- For Part 2 only: a **VPS** with Docker, and a GitHub account (you fork and click one button).

---

## Part 1 — run the store on your laptop (2 minutes)

The store binary (`pos_edge`) runs with no database, no hardware, and no cloud — it uses in-memory
fakes. This is the fastest way to see the framework work.

```bash
git clone <your fork url> && cd pos-framework-4ps
just run-edge          # or: cargo run -p minimal-edge
```

Open **http://127.0.0.1:8787/**. Open a table, add items from two browser tabs at once, fire a course,
split the bill, take payment. **Unplug your network** — it keeps working. That is the core promise:
a store never stops selling. `Ctrl-C` stops it.

Nothing was installed, nothing was configured. If this worked, the framework is healthy on your machine.

---

## Part 2 — deploy the cloud to a VPS (~15 minutes)

The cloud binary (`pos_cloud`) is the back office: admin, dashboards, config, the public API, QR
ordering. It runs as a small Docker Compose stack on **one VPS**. You never type a command on the
server — CI ships it over SSH and the box mints its own secrets ([ADR-0044](../adr/0044-fork-and-deploy.md)).

**The fastest path — no domain, no Cloudflare:**

1. Get any **VPS** (~1.5 GB RAM: DigitalOcean, Vultr, Hetzner, AWS Lightsail, GCP CE…), Ubuntu, with
   Docker + the Compose plugin installed. Note its **public IP**.
2. In your GitHub fork: **Settings → Secrets and variables → Actions**, add these **6 secrets**:

   | Secret | Value |
   |---|---|
   | `VPS_HOST` | the VPS IP or hostname |
   | `VPS_USER` | the SSH user (`root`, or a sudo deploy user) |
   | `VPS_SSH_KEY` | that user's **private** key (the whole PEM) |
   | `VPS_KNOWN_HOSTS` | output of `ssh-keyscan <VPS_HOST>` (so it is not trust-on-first-use) |
   | `DOMAIN` | `<vps-ip-with-dashes>.sslip.io`, e.g. `203-0-113-9.sslip.io` |
   | `ACME_EMAIL` | your email (for the TLS certificate) |

   `sslip.io` resolves that hostname to your IP for free, so you get **real HTTPS with no DNS setup**
   — Caddy issues the certificate over HTTP-01/TLS-ALPN using the stock official image (no Cloudflare,
   no plugin), so make sure the VPS's **`:80` and `:443` are reachable from the internet**. (With your
   own domain instead, set `DOMAIN` to it and add `CF_DNS_API_TOKEN` for DNS-01 via Cloudflare; see the
   [deploy runbook](../deploy-runbook.md).)
   **SSH on a non-default port?** Add a `VPS_PORT` secret (it defaults to 22 when unset), and
   generate the host key with the port: `ssh-keyscan -p <port> <VPS_HOST>`.
3. **Settings → Environments → New environment → `production`**, and add yourself as a **required
   reviewer**. Every deploy pauses for one approval — that reviewer is also the second person the
   admin break-glass needs.
4. **Actions → deploy → Run workflow** (leave `reset_admin` off), approve it. In ~15 minutes the stack
   is up. The run log prints a **one-time super-admin setup token** — copy it.
5. Enrol yourself:
   ```bash
   curl -X POST https://<your DOMAIN>/admin/setup \
     -H 'content-type: application/json' \
     -d '{"setup_token":"<token from the log>","password":"<a strong 12+ char passphrase>"}'
   ```
   The response returns an `otpauth://…` URI once — add it to an authenticator app. Sign in at
   `https://<your DOMAIN>/admin/login`. Done.

The full list of secrets (including the with-a-domain and backup options), rollback, and the
break-glass are in the **[deploy runbook](../deploy-runbook.md)** — the single source of truth for
deployment.

### Running the cloud locally instead

You usually do **not** run the cloud on your laptop — it is meant to be deployed. If you need to for
development, it wants PostgreSQL/NATS/Garage (start them with
`docker compose -f deploy/compose.yml up -d postgres nats garage`) and a config file named by
`POS_CLOUD_CONFIG`; then `just run-cloud`. The deploy runbook covers the config.

---

## Where to next

- **Connect a real payment terminal, courier, or marketplace** → [Write an adapter](write-an-adapter.md).
- **Support a new country** (tax invoices, locale, local vendors) → [Add a country module](add-a-country-module.md).
- **Check the capacity numbers / run fleet scenarios** → [Run the simulator](run-the-simulator.md).
- **Understand the whole system** → [`docs/architecture.md`](../architecture.md).
