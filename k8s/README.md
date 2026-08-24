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
