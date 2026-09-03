# Kubernetes lane (optional)

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20

The **default** way to run a country cell is Docker Compose on one VPS
([ADR-0044](../docs/adr/0044-fork-and-deploy.md), [`deploy/`](../deploy/README.md)). This directory is
the **optional** lane for operators who already run a cluster: it mirrors the same four backends —
`pos_cloud`, PostgreSQL, NATS (JetStream), Garage — and the same secret model. It is **not** the path
the P8 exit criterion measures, and [`deploy/`](../deploy/README.md) stays the supported default.

[`pos-cloud.yaml`](pos-cloud.yaml) is a **starting skeleton**, not a turnkey chart. Adapt the
environment-specific bits, each marked `# ADAPT` in the file: the image references, the
`storageClassName`, the `ingressClassName`, and the TLS issuer.

## Secrets — created in the cluster, never committed

The same rule as Compose: operational secrets live in the cluster, never in Git
([ADR-0044](../docs/adr/0044-fork-and-deploy.md)). Create them with `kubectl`, filling the values a
human or a bootstrap Job generates (the equivalents of `deploy/secrets/*`):

```
kubectl create namespace pos-cloud

# pos_cloud config (bind, database_url with the postgres password, the one-time admin_setup_token)
kubectl -n pos-cloud create secret generic pos-cloud-config --from-file=cloud.toml=./cloud.toml

# postgres credentials
kubectl -n pos-cloud create secret generic postgres-credentials \
  --from-literal=POSTGRES_USER=pos \
  --from-literal=POSTGRES_PASSWORD="$(openssl rand -hex 24)" \
  --from-literal=POSTGRES_DB=poscloud

# nats and garage server configs
kubectl -n pos-cloud create secret generic nats-config   --from-file=nats.conf=./nats.conf
kubectl -n pos-cloud create secret generic garage-config --from-file=garage.toml=./garage.toml
```

`cloud.toml` must set `database_url` to reach the in-cluster Postgres Service
(`host=postgres.pos-cloud.svc.cluster.local`) and carry the one-time `admin_setup_token`; first-boot
enrolment then works exactly as in the runbook ([ADR-0045](../docs/adr/0045-first-boot-admin-enrolment.md),
[`docs/deploy-runbook.md`](../docs/deploy-runbook.md) step 4).

## The `/internal` deny is mandatory, and you must verify it

`pos_cloud` serves three routes under `/internal/*` that are documented as private-network-only
(`/internal/ingest` is the reconciliation re-push, `/internal/reconcile` the id-diff endpoint,
`/internal/ota/report` the fleet report). Since ADR-0097 they also require the
`X-Pos-Internal-Key` shared secret from `cloud.toml` — **which does not make this deny optional.**
The two controls answer different questions: the key decides who inside the network may call them,
this deny decides whether the internet can reach them at all. Reaching them from outside means event
injection, an id-holding oracle, and falsifiable fleet state for any tenant, behind nothing but one
shared key.

The Compose lane closes them in [`deploy/Caddyfile.d/site.caddy`](../deploy/Caddyfile.d/site.caddy),
and `cargo run -q -p xtask -- tls-modes` fails if that deny is ever removed. **This lane did not have
the equivalent**: its `Ingress` routed `/` as a single prefix, so all three were public. The skeleton
now carries an ingress-nginx `server-snippet` returning 404 on `/internal/`, and the same gate checks
it is still there.

An allow-list of public prefixes would be fail-closed and need no annotation, but it is not available
here: the console is a single-page app served from the `/` catch-all, so every client-routed path has
to reach the backend. Denying the one private prefix is the only shape that works.

**Verify it, do not assume it.** ingress-nginx ignores snippet annotations when the controller runs
with `allow-snippet-annotations: false` — its default since 1.9 — and it does so *silently*, which
looks exactly like success. After `kubectl apply`:

```
curl -s -o /dev/null -w '%{http_code}\n' https://cloud.example.com/internal/reconcile
```

It must print `404`. If it prints anything else, your controller is not applying the snippet and you
must implement the deny another way — a front proxy, a controller-native route rule, or a middleware
CRD (Traefik: a `Middleware`; HAProxy: `http-request deny`). **Do not run this lane in production
until that command prints 404.** It belongs in the gate register as a per-deployment human check.

Denying at the proxy is the control this lane has today. Whether the routes should *also* carry a
shared secret at the application layer is open ([task #275](../docs/roadmap-v3.md)); until it is
decided, the proxy deny is the only thing between them and the internet, in both lanes.

## TLS

Use your cluster's own ingress + certificate machinery — e.g. an `Ingress` with cert-manager issuing
over **DNS-01** through the Cloudflare token, grey-clouded record, **never Cloudflare "Flexible"**
([ADR-0023](../docs/adr/0023-tenant-hostname-and-slug.md)). The origin always terminates real TLS. The
skeleton's `Ingress` carries placeholder annotations to fill in.

## Apply

```
kubectl apply -f k8s/pos-cloud.yaml
```

## What this lane does not cover

Backups and the restore drill ([ADR-0046](../docs/adr/0046-backups-and-restore.md)) are written for
the Compose lane (`deploy/backup.sh`, `deploy/restore-drill.sh`). On Kubernetes, run the equivalent as
a `CronJob` against the Postgres Service — the same `pg_dump` / restore-and-reconcile logic — and ship
off-box the same way. That CronJob is left to the operator, since backup targets and schedules are
cluster-specific.
