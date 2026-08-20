# ADR-0046 — Cloud backups and the restore drill

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0003](0003-cattle-not-pets.md) · [ADR-0016](0016-postgres-access.md) · [ADR-0031](0031-cloud-adapter-transports.md) · [ADR-0044](0044-fork-and-deploy.md) · `docs/roadmap.md` P8

**Context.** The cell is cattle, not a pet ([ADR-0003](0003-cattle-not-pets.md)): the box is
reproducible from this repo, and everything that is *not* reproducible — the event log, the rollups,
the config tree, the admin and API-key rows — lives in the PostgreSQL and Garage volumes
([ADR-0044](0044-fork-and-deploy.md)). Losing that data loses the chain's memory. So P8 owes the
durability half of fork-and-deploy: continuous archiving so a crash loses minutes not days, periodic
snapshots that are actually restorable, a copy that survives losing the whole box, and — the part
that is usually skipped and therefore the part that matters — a drill that proves a backup restores,
because *a backup that has never been restored is not a backup*.

**Decision.**

- **WAL archiving plus periodic base dumps — both needed, neither alone.** PostgreSQL runs with
  `archive_mode=on` and a WAL archive, so recovery point is one segment (minutes), not a day. But a
  WAL stream is useless without a base to replay onto, so a scheduled `pg_dump -Fc` of the cloud
  database is taken too — a single restorable artifact. WAL gives the recency; the dump gives the
  floor.

- **Every backup is shipped off-box; on-box alone does not count.** A dump that lives only on the
  box the database is on dies with that box. Backups land first on local disk (fast) and are then
  copied to an **off-box second tier** with `rclone` (the `RCLONE_*` secrets from
  [ADR-0044](0044-fork-and-deploy.md)). Garage holds objects (menu images) on-box; those too sync
  off-box, at a lower cadence because they are the most regenerable.

- **Four backup classes, deliberately unequal.** Not all data is worth the same RPO or retention, so
  the archive's four classes are honoured rather than flattened into one nightly job:
  1. **Continuous WAL** — the event log's recency, minutes of RPO, shipped as segments fill.
  2. **Daily cloud-database dump** — the whole system of record (events, rollups, config tree, admin,
     API keys, webhooks) in one `pg_dump`, off-box, long retention.
  3. **Garage object sync** — menu images, weekly, off-box; lowest value because they regenerate from
     the tenant's source uploads.
  4. **The `.pre-update` snapshot** — a dump taken *immediately before every deploy* brings up a new
     image, kept short-term on-box, so a bad release rolls back to a known-good database at once.

- **A weekly restore drill, covering both halves, wired into `nightly.yml`.** The drill restores the
  latest cloud-database dump into a throwaway database and reconciles row counts against the source —
  proving the dump is not just written but *loadable*. The archive requires the drill cover **both** a
  random store backup **and** the cloud database; the store half is edge WAL shipping, which lands
  with the machine-replacement work (`docs/roadmap.md` P9, spike A4), so this ADR builds the cloud-DB
  half now and the drill grows its store half there. The scripts are exercised in CI against a
  synthetic dataset (a service Postgres); the real weekly drill on production data is a box cron.

**Rejected.**

- **On-box backups only** — rejected: a box loss loses the database and its backups together, which
  is not a backup at all. The off-box tier is the whole point.
- **WAL archiving with no base dump** — rejected: WAL segments replay *onto* a base; without a
  periodic base, the archive cannot be restored, only accumulated.
- **A backup job with no restore drill** — rejected as the classic false comfort: the failure mode is
  a backup that has silently been unrestorable for months. The drill, not the backup, is the
  deliverable that proves recovery.
- **One nightly dump for everything, one retention** — rejected: it over-pays for regenerable objects
  and under-protects the event log. The four classes price each by its real value.

**Consequences.**

- `deploy/` gains `backup.sh` (dump + off-box ship, and the `--label pre-update` mode) and
  `restore-drill.sh` (restore-and-reconcile); `compose.yml`'s Postgres enables WAL archiving to a
  volume; the deploy workflow takes the `.pre-update` snapshot before bring-up; `nightly.yml`'s
  `restore-drill` job runs the drill for real against a service Postgres.
- Recovery is now bounded: minutes of data at risk (WAL), a daily floor, an instant pre-deploy
  rollback, and a weekly proof that the floor restores. The full machine-replacement promise — the
  store half, WAL shipping on Windows — remains P9 per spike A4; this is the cloud half it builds on.
- Like the rest of P8, the scripts cannot be run end to end in this repo's CI environment (no Docker
  daemon); they are validated by `bash -n`, the compose/workflow YAML parse, and the nightly drill's
  own run. The true proof is a human restoring a real backup — which is exactly what the weekly drill
  automates.
