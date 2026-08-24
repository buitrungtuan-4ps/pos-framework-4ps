# ADR-0044 — Fork-and-deploy: one VPS, Docker Compose, secrets generated on the server

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0003](0003-cattle-not-pets.md) · [ADR-0023](0023-tenant-hostname-and-slug.md) · [ADR-0016](0016-postgres-access.md) · [ADR-0031](0031-cloud-adapter-transports.md) · `docs/roadmap.md` P8

**Context.** `docs/roadmap.md` P8 is fork-and-deploy: someone forks this repository, sets a handful of
secrets, runs one workflow, and ~15 minutes later has a working cloud — admin UI live behind HTTPS,
**with no command typed on the server**. The cloud is one country cell on **one VPS** (Kubernetes was
rejected for the default; `docs/roadmap.md` "what is already settled"), running the four backends P7
already targets: `pos_cloud`, PostgreSQL, NATS JetStream, Garage. The open questions are the shape of
the deployment artifacts and, above all, **where the operational secrets come from** — because the
one thing a fork must never do is commit a database password or a signing key to GitHub.

**Decision.**

- **Docker Compose on a single VPS is the default; `k8s/` is an optional lane.** Four services in one
  `deploy/compose.yml`, sized to fit a small VPS (~1.2–1.5 GB across the four): `pos_cloud` (the
  binary this repo builds), `postgres`, `nats` with JetStream, `garage`. Each backend is exactly the
  one P7 chose ([ADR-0016](0016-postgres-access.md), [ADR-0031](0031-cloud-adapter-transports.md)).
  Compose is the whole orchestration — no control plane to run — which is the "cattle, not pets"
  posture ([ADR-0003](0003-cattle-not-pets.md)) at cloud scale: the box is reproducible from this repo
  plus its data volumes. A `k8s/` manifest set mirrors it for operators who already run a cluster, but
  it is not the path the exit criterion measures.

- **Operational secrets are generated on the server and never returned to GitHub.** `bootstrap.sh`
  (P8b) mints the *internal* secrets — the PostgreSQL password, the NATS credentials, the Garage
  access keys, and the `pos_cloud` config — **on the VPS**, writes them into a `600` env file Compose
  reads, and prints nothing secret to stdout. GitHub holds only the **4–6 secrets needed to reach the
  box and issue a certificate** — `VPS_HOST`, `DOMAIN`, `ACME_EMAIL`, `CF_DNS_API_TOKEN`, the
  `RCLONE_*` off-box backup credentials — never an application secret. So a leaked GitHub token exposes
  the deploy channel, not the database, and rotating an internal secret is a server-side action, not a
  repository edit.

- **Caddy terminates TLS and reverse-proxies to `pos_cloud`.** A tiny `deploy/Caddyfile` fronts the
  stack: Caddy obtains and renews a certificate over **DNS-01** (the `CF_DNS_API_TOKEN`), so the box
  needs no inbound `:80` reachable to ACME and the record can stay **grey-clouded** (DNS-only) at
  Cloudflare. With no purchased domain, `DOMAIN=<vps-ip>.sslip.io` gives a real hostname that resolves
  to the box for a real certificate. Cloudflare's **"Flexible" SSL is forbidden outright** — it serves
  the browser HTTPS while fetching the origin over plaintext, which is a downgrade masquerading as
  encryption; the origin is always real TLS (Caddy) and Cloudflare, if orange-clouded at all, must be
  Full (strict). This is the "redirect, never proxy, and never Flexible" rule
  [ADR-0023](0023-tenant-hostname-and-slug.md) fixed for tenant hostnames, applied to the cell's own host.

- **Images are version-pinned in the repo and digest-locked at fork.** Every service names an
  **immutable version tag** (e.g. `postgres:16.4-bookworm`, not `postgres:16`), and the fork's first
  deploy records the resolved `sha256` digest into the env file — a hard-coded digest cannot be
  written blindly into the repo, because it is registry-specific and unknowable without pulling, and a
  wrong digest fails closed. `pos_cloud` itself is built by `deploy/Dockerfile` (multi-stage: a Rust
  builder, a slim runtime carrying only the binary and CA roots) and deployed by **immutable tag**, so
  a **rollback is re-running the workflow with an older tag** — the app container is stateless; all
  durable state is in the `postgres`/`garage`/JetStream volumes.

**Rejected.**

- **Kubernetes as the default** — rejected (settled in `docs/roadmap.md`): a control plane to run and
  patch is pets-not-cattle overhead for one VPS per cell. It stays an optional lane for operators who
  already have a cluster.
- **Secrets in GitHub Actions secrets / committed `.env`** — rejected: it puts the database password
  and signing keys one token-leak away, and couples secret rotation to a repository edit. The box
  generates its own; GitHub gets only what it needs to *reach* the box.
- **Cloudflare "Flexible" SSL, or an HTTP origin** — rejected outright: plaintext between the edge
  proxy and the origin is an unencrypted hop the padlock hides. Caddy always terminates real TLS.
- **A `latest` tag or floating major** (`postgres:16`) — rejected: a redeploy must be the *same* bits,
  and a rollback must be a *specific* older build. Floating tags make both non-deterministic.

**Consequences.**

- `deploy/` gains `Dockerfile`, `compose.yml`, `Caddyfile`, and (P8b) `bootstrap.sh`; `k8s/` (P8e) is
  the optional lane. The deploy workflow (P8c) ships the image and runs bootstrap over the existing
  SSH channel; backups and the restore drill (P8d) are the durability half.
- This environment has no Docker daemon, so the artifacts are validated by what tooling allows — YAML
  parses, `bash -n`, and the `actions-pinned` gate on the workflow — and by review; the true
  end-to-end check is the P8 exit itself: a human forks, sets the secrets, runs the workflow, and
  reaches the admin UI. That is the one layer the roadmap always said needs a human.
- Nothing here changes application code: P8 is packaging and operations around the `pos_cloud` binary
  P7 already produced.
