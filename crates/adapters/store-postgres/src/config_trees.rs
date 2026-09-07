// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The configuration-tree table over PostgreSQL (P7, [ADR-0033](../../../docs/adr/0033-config-tree.md)).
//!
//! One row per `(tenant, store)`; the row's `state` column is the whole `ConfigTreeState` — the four
//! authored layers and the published version history — held as `jsonb`. This adapter keeps only the
//! SQL and hands back the raw JSON text; `pos-cloud` implements its `ConfigTreeStore` seam over this
//! type and does the `ConfigTreeState` (de)serialisation, so no cloud-domain type leaks into the
//! adapter — the same split the rollup table uses.
//!
//! Tenant isolation is the `(tenant_id, store_id)` key: a load names both, so it can only ever return
//! the caller's own tenant's row (the migration also enables RLS as a second line for a query role).

use deadpool_postgres::Pool;

use pos_ports::PortError;
use pos_proto::ids::{StoreId, TenantId};

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// The config-tree store over a shared pool. Built by [`PostgresStore::config_trees`](crate::PostgresStore::config_trees).
#[derive(Clone, Debug)]
pub struct PostgresConfigTrees {
    pool: Pool,
}

/// What a conditional bump did.
///
/// `Result<StoredBump, PortError>` has no arm for a well-formed request the *row's state* refuses,
/// and both refusals here are exactly that: the caller can fix them and retry, where `PortError`
/// funnels to `503 the service is unavailable` and invites retrying the same losing request. The
/// same reasoning [`UpdateOutcome`](../../../pos-cloud/src/version.rs) applies to conditional writes
/// on the config tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BumpOutcome {
    /// The generation was issued.
    Issued(StoredBump),
    /// The row is not at the generation the caller's `If-Match` named — someone else bumped in
    /// between, or the caller is holding a stale read. Answers `412`.
    ///
    /// `current` is the generation the row is actually at, or `None` when the store has no lease
    /// row at all and the caller named a number rather than `*`. It is read by a second statement
    /// *after* the refusal, so another admin can move it again in between: the number is advisory,
    /// for the message. The decision was made by the write's own `WHERE`, which cannot race.
    VersionMismatch {
        /// The generation the row is actually at, or `None` when there is no row.
        current: Option<i64>,
    },
    /// A handover is still in flight: `superseded_generation` names a machine whose events this
    /// cloud has never seen, and the request did not acknowledge that generation. Answers `422`
    /// ([ADR-0096](../../../../docs/adr/0096-unprocessable-status.md)).
    ///
    /// Advisory in the same way and for the same reason as `current` above.
    Undrained {
        /// The generation whose machine still owes this cloud events.
        superseded: i64,
    },
}

/// What settling a handover by hand did
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredSettle {
    /// `superseded_generation` held the named value and is now null.
    Settled,
    /// It held something else — `current`, or `None` for a handover already settled. Answers `422`.
    ///
    /// Advisory in the same way `BumpOutcome::VersionMismatch { current }` is: read after the
    /// refusal, so it can be stale by the time it is rendered. The decision was the write's `WHERE`.
    NotSuperseded {
        /// What the column holds now.
        current: Option<i64>,
    },
    /// There is no lease row for this store, so no handover to settle. Answers `404`.
    NoLease,
}

/// What retiring a handover did
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredRetire {
    /// The decision is recorded on the row.
    Retired,
    /// A handover is still in flight, so the outgoing machine may still hold events. Answers `422`.
    Undrained {
        /// The generation whose machine has not been proved drained.
        superseded: i64,
    },
    /// This handover is already retired. Answers `422` rather than overwriting the first decision.
    AlreadyRetired {
        /// Unix ms of the recorded decision.
        at: i64,
        /// The deciding admin's id.
        by: String,
    },
    /// The write refused, yet the row satisfies both of its conditions by the time it was read:
    /// another admin resolved the race in between. The caller re-reads and retries. Answers `409`.
    ///
    /// Reported rather than retried here, because a retry from inside the adapter would be a second
    /// attempt at a decision the caller made once, against a row that has changed underneath them.
    Raced,
    /// The store is on generation `0`, its first-ever lease, so no machine has ever been replaced
    /// and there is no handover to retire. Answers `422`.
    ///
    /// Distinct from [`Self::NoLease`], which is a store the cloud has never issued a lease at all:
    /// this store has one, and it is the only one it has ever had.
    NeverSuperseded,
    /// There is no lease row for this store, so no handover to retire. Answers `404`.
    NoLease,
}

/// What one lease bump wrote: the generation it issued and the edge placement the store now has
/// ([ADR-0108](../../../../docs/adr/0108-the-lease-generation-is-authority.md),
/// [ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// The two travel together because one statement wrote them, and a caller that took only the
/// generation would have to read the placement back in a second query — reopening exactly the
/// window ADR-0110 closed by making them one write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBump {
    /// The generation just issued. `0` for a store's first-ever lease.
    pub generation: i64,
    /// The store's edge placement as an `EDGE_PLACEMENT_*` token, whether this bump set it or kept
    /// the one already there. Read from `RETURNING`, never echoed from the request.
    pub edge_placement: String,
    /// The generation this bump displaced, or `None` for a store's first-ever lease — which
    /// supersedes nobody ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// It stays set until something proves the old machine drained: a heartbeat reporting *that*
    /// generation with an empty outbox, or an admin who read a powered-off machine's outbox and said
    /// so. A store with no handover in flight carries `None`, and so does one that has never been
    /// bumped — the two are the same to a reader, because neither has a machine owing events.
    pub superseded_generation: Option<i64>,
    /// The store's region country as an ISO 3166-1 alpha-2 code, or `None` for an in-store
    /// placement — which has none, because the machine is in the shop
    /// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Read from `RETURNING` beside the placement, never echoed from the request, and for the same
    /// reason: a bump that named no placement keeps the region it had.
    pub region_country: Option<String>,
    /// The store's region label — the place a person recognises. Never parsed by anything here.
    ///
    /// Always present exactly when `region_country` is: the write sets and clears the pair
    /// together, and there is no statement that touches one alone.
    pub region_label: Option<String>,
}

/// What one bump does to the store's two region columns
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// The adapter's mirror of `pos_cloud::lease::RegionWrite`, taking borrowed strings because the
/// caller already owns them and this is a statement parameter, not a stored value. Three variants
/// and no fourth: a hosted placement with no region is not expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredRegionWrite<'a> {
    /// The bump named no placement, so it is a swap in place and the stored region stays exactly as
    /// it is. Every bump that existed before these columns did.
    Keep,
    /// The bump moved the store in-store, which has no region: both columns go to NULL.
    Clear,
    /// The bump moved the store to a hosted placement, and this is where.
    Set {
        /// ISO 3166-1 alpha-2, already upper-cased by the caller's `CountryCode::parse`.
        country: &'a str,
        /// The place a person recognises, already trimmed and length-checked by the caller.
        label: &'a str,
    },
}

impl<'a> StoredRegionWrite<'a> {
    /// The three statement parameters this write becomes: whether to touch the columns at all, and
    /// what to put in them.
    ///
    /// `Keep` answers `false` and the `CASE` in the statement leaves both columns alone; the two
    /// NULLs beside it are what the *insert* branch writes, and a store with no row has no region
    /// to keep, so NULL is right there too.
    const fn parameters(self) -> (bool, Option<&'a str>, Option<&'a str>) {
        match self {
            Self::Keep => (false, None, None),
            Self::Clear => (true, None, None),
            Self::Set { country, label } => (true, Some(country), Some(label)),
        }
    }
}

impl PostgresConfigTrees {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Loads a store's tree state as the raw JSON text of a `ConfigTreeState` **and the version the
    /// row was read at**, or `None` if the `(tenant, store)` pair has no row yet.
    ///
    /// The version is `xmin::text`, the same opaque token every other conditional write in this
    /// adapter uses ([ADR-0094](../../../../docs/adr/0094-console-optimistic-concurrency.md)). It is
    /// not the tree's `ConfigVersionId`: that one lives *inside* the document and is the caller's
    /// concern, while this one is the row's and is what [`Self::save_state`] compares.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn load_state(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<(String, String)>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT state::text, xmin::text FROM config_trees \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| (row.get(0), row.get(1))))
    }

    /// Writes a store's tree state (the raw `ConfigTreeState` JSON) **only if the row is still at
    /// `expected`** ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
    ///
    /// This replaces an unconditional upsert, and the two cases are deliberately different
    /// statements rather than one `ON CONFLICT` that papers over them:
    ///
    /// - `expected = None` — the caller read no row, so this must *create* one. `ON CONFLICT DO
    ///   NOTHING` returns zero rows if another publish created it first, which is a
    ///   [`RowUpdate::VersionMismatch`], not a silent overwrite. An upsert here would clobber that
    ///   other publish entirely.
    /// - `expected = Some(v)` — the row must still be at `v`. Zero rows means either the version
    ///   moved or the row is gone, and the probe on the failure path separates them, exactly as the
    ///   record-shaped writes in this adapter do.
    ///
    /// The `$3::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the
    /// same reason the rollup and event tables cast their bound documents. The comparison is on
    /// `xmin::text` rather than a cast of `expected` to `xid`, because casting caller-supplied text
    /// to `xid` raises `invalid input syntax for type xid` and would turn a stale token into a `500`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn save_state(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        state_json: &str,
        expected: Option<&str>,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let tenant_text = tenant.to_string();
        let store_text = store_id.to_string();

        let Some(expected) = expected else {
            let inserted = connection
                .query_opt(
                    "INSERT INTO config_trees (tenant_id, store_id, state) \
                     VALUES ($1, $2, $3::text::jsonb) \
                     ON CONFLICT (tenant_id, store_id) DO NOTHING \
                     RETURNING xmin::text",
                    &[&tenant_text, &store_text, &state_json],
                )
                .await
                .map_err(unavailable)?;
            return Ok(inserted.map_or(RowUpdate::VersionMismatch, |row| {
                RowUpdate::Updated(row.get(0))
            }));
        };

        let updated = connection
            .query_opt(
                "UPDATE config_trees SET state = $3::text::jsonb, updated_at = now() \
                 WHERE tenant_id = $1 AND store_id = $2 \
                 AND xmin::text = $4 RETURNING xmin::text",
                &[&tenant_text, &store_text, &state_json, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }

        // Zero rows is ambiguous on its own: the version moved, or the row is not there. The probe
        // is what makes a conflict distinguishable from an absence.
        let present = connection
            .query_opt(
                "SELECT 1 FROM config_trees WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant_text, &store_text],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
    }

    /// Upserts a store's liveness row from a config pull ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md)):
    /// records the contact instant, the config version the edge reported holding, and that this
    /// contact was a config pull. `held_version` is the raw ULID string the edge sent, or `None` if it
    /// holds nothing yet. `seen_at_ms` is Unix milliseconds.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_seen(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        held_version: Option<&str>,
        seen_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO store_liveness \
                 (tenant_id, store_id, last_seen_at, config_version_held, last_config_pull_at) \
                 VALUES ($1, $2, $3, $4, $3) \
                 ON CONFLICT (tenant_id, store_id) DO UPDATE SET \
                 last_seen_at = EXCLUDED.last_seen_at, \
                 config_version_held = EXCLUDED.config_version_held, \
                 last_config_pull_at = EXCLUDED.last_config_pull_at",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &seen_at_ms,
                    &held_version,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Records a store's OTA report ([ADR-0078](../../../../docs/adr/0078-sync-and-ota-closure.md)): the
    /// version it is now running and whether its self-test passed, onto the liveness read model. A
    /// report is contact, so it advances `last_seen_at` too (which lets a fresh row satisfy the
    /// `NOT NULL` on `last_seen_at` when a store reports before it has ever pulled config). `reported_at_ms`
    /// is Unix milliseconds.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_ota_report(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        installed: &str,
        self_test_passed: Option<bool>,
        reported_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO store_liveness \
                 (tenant_id, store_id, last_seen_at, installed_version, self_test_ok, reported_at) \
                 VALUES ($1, $2, $3, $4, $5, $3) \
                 ON CONFLICT (tenant_id, store_id) DO UPDATE SET \
                 last_seen_at = EXCLUDED.last_seen_at, \
                 installed_version = EXCLUDED.installed_version, \
                 self_test_ok = EXCLUDED.self_test_ok, \
                 reported_at = EXCLUDED.reported_at",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &reported_at_ms,
                    &installed,
                    &self_test_passed,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Issues this store's next lease generation and returns it, together with the edge placement
    /// the store now has — a **bump**, and the only write this adapter offers
    /// ([ADR-0108](../../../../docs/adr/0108-the-lease-generation-is-authority.md),
    /// [ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// A store with no row starts at generation `0`, which ADR-0049 names as "the first lease a
    /// store ever issues"; an existing row moves to `generation + 1`. There is deliberately no
    /// set-to-a-value and no decrement: an authority that takes a number from its caller is not one,
    /// and a generation that can move backwards is not monotonic, which is the entire mechanism.
    ///
    /// `edge_placement` is `Some` when the bump is moving the store to a different machine and
    /// `None` when it is replacing the machine in place — ADR-0003's swap — in which case the store
    /// keeps the placement it had. It is written **in this same statement**, which is why the row
    /// and the generation beside it can never disagree: there is no window between them to read.
    ///
    /// `region` is the same fact one level further out
    /// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)): where in the
    /// world the machine holding the new generation is. It rides the same statement for the same
    /// reason, and a caller cannot express the one state ADR-0114 exists to prevent — a hosted
    /// placement with no region — because [`StoredRegionWrite`] has no variant for it.
    ///
    /// One statement, so two admins bumping at once serialise on the row rather than racing to the
    /// same number.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn bump_store_lease(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        issued_at_ms: i64,
        edge_placement: Option<&str>,
        region: StoredRegionWrite<'_>,
        acknowledge_undrained: Option<i64>,
        expected_generation: Option<i64>,
    ) -> Result<BumpOutcome, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        // `$4` is NULL for a bump that names no placement. On insert the column falls to its schema
        // default (`IN_STORE`, what every store in the fleet already is); on conflict `COALESCE`
        // keeps the stored value. Reading the placement back out of `RETURNING` rather than echoing
        // the argument is deliberate: the caller audits what the store *now is*, which for a
        // no-placement bump is a value it never sent.
        //
        // **Two CTEs, not an upsert, and that is a correction rather than a preference.** The
        // obvious shape — one `INSERT … ON CONFLICT DO UPDATE` with the preconditions on the
        // conflict branch — cannot express this. Guarding the insert with `WHERE $6 IS NULL` gates
        // the *whole statement*: with a numbered `If-Match` the SELECT yields no row, so there is
        // nothing to insert, so no conflict arises, so `DO UPDATE` never runs, and every bump after
        // a store's first is silently refused. Only a real database showed that — the shape
        // type-checks and reads correctly.
        //
        // So the update is its own statement, conditional on both preconditions, and the insert is
        // a second one conditional on the caller having claimed there is no row (`If-Match: *`).
        // Exactly one can produce a row, so the union yields one row or none, and none is
        // unambiguously a refusal.
        //
        // `superseded_generation = generation` records the number this bump displaces (ADR-0110).
        // Every `SET` expression in one `UPDATE` reads the *pre-update* row — the same fact
        // `generation = generation + 1` beside it depends on — so the recorded value is exact by
        // construction, with nothing to race. The insert writes NULL explicitly: a store's first
        // lease supersedes nobody.
        //
        // `DO NOTHING` covers two callers creating one store at once: the loser returns no row and
        // is reported as a version mismatch, which is true — somebody else issued the lease — rather
        // than a unique-violation 503 that would invite the same losing retry.
        //
        // Why the column is needed at all: `store_liveness` holds one row per store, so the instant
        // the incoming machine heartbeats, the outgoing machine's final `outbox_depth` is gone. This
        // is the cloud's only durable memory of the question \"has N drained?\".
        //
        // `retired_at`/`retired_by` are cleared in the same breath, and that is not tidiness. They
        // describe *this* store's current handover, and a bump starts a new one with a new outgoing
        // machine. Left in place they would say a machine still in the shop had been decided
        // unnecessary. Every retirement's history is the audit trail's, which keeps it.
        let requested = edge_placement.map(ToOwned::to_owned);
        // `$7` says whether this bump *decided* the region; `$8`/`$9` are what it decided. A swap in
        // place sends `false` and the `CASE` leaves both columns exactly as they are, which is what
        // keeps every bump written before these columns existed behaving identically. On the insert
        // branch there is nothing to keep — a store with no lease row has no region — so the pair
        // goes in directly, and `Keep` puts NULL there, which is the truth for a first-ever lease.
        //
        // Both columns are always written together. There is no statement anywhere that touches one
        // without the other, which is what makes "a hosted placement always has a region, an
        // in-store one never does" a property of the schema's *writes* rather than a rule a reader
        // has to trust.
        let (region_decided, region_country, region_label) = region.parameters();
        let row = connection
            .query_opt(
                "WITH bumped AS ( \
                     UPDATE store_lease SET \
                     generation = generation + 1, \
                     superseded_generation = generation, \
                     retired_at = NULL, \
                     retired_by = NULL, \
                     issued_at = $3, \
                     edge_placement = COALESCE($4, edge_placement), \
                     region_country = CASE WHEN $7 THEN $8 ELSE region_country END, \
                     region_label = CASE WHEN $7 THEN $9 ELSE region_label END \
                     WHERE tenant_id = $1 AND store_id = $2 \
                       AND generation = $6::bigint \
                       AND (superseded_generation IS NULL \
                            OR superseded_generation = $5::bigint) \
                     RETURNING generation, edge_placement, superseded_generation, \
                               region_country, region_label \
                 ), issued AS ( \
                     INSERT INTO store_lease \
                     (tenant_id, store_id, generation, issued_at, edge_placement, \
                      superseded_generation, region_country, region_label) \
                     SELECT $1, $2, 0, $3, COALESCE($4, 'EDGE_PLACEMENT_IN_STORE'), NULL, $8, $9 \
                     WHERE $6::bigint IS NULL \
                     ON CONFLICT (tenant_id, store_id) DO NOTHING \
                     RETURNING generation, edge_placement, superseded_generation, \
                               region_country, region_label \
                 ) \
                 SELECT * FROM bumped UNION ALL SELECT * FROM issued",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &issued_at_ms,
                    &requested,
                    &acknowledge_undrained,
                    &expected_generation,
                    &region_decided,
                    &region_country,
                    &region_label,
                ],
            )
            .await
            .map_err(unavailable)?;

        let Some(row) = row else {
            // No row came back, so nothing was written — both branches of the statement are
            // conditional. Which refusal it was is not encoded in the absence, so read the row once
            // to choose the message. Racy by construction: another admin can bump between the
            // refusal and this read. That is acceptable because the *decision* was already made,
            // atomically, by the statement's own `WHERE`; this only picks what to say about it.
            let probe = connection
                .query_opt(
                    "SELECT generation, superseded_generation FROM store_lease \
                     WHERE tenant_id = $1 AND store_id = $2",
                    &[&tenant.to_string(), &store_id.to_string()],
                )
                .await
                .map_err(unavailable)?;
            let Some(probe) = probe else {
                // No lease row at all, and the caller named a generation rather than `*` — the
                // insert's own `WHERE` refused it. `*` would have inserted.
                return Ok(BumpOutcome::VersionMismatch { current: None });
            };
            let current: i64 = probe.get(0);
            let superseded: Option<i64> = probe.get(1);
            // Order matters, and this is the order the statement itself evaluates: the generation
            // check comes first, so a caller holding a stale read is told *that* rather than being
            // sent to acknowledge a handover it has not seen yet.
            if Some(current) != expected_generation {
                return Ok(BumpOutcome::VersionMismatch {
                    current: Some(current),
                });
            }
            return Ok(match superseded {
                Some(superseded) => BumpOutcome::Undrained { superseded },
                // The row satisfies both conditions now, so the refusal was a race that has since
                // resolved. Report it as a version mismatch: the caller re-reads and retries, which
                // is the correct next move either way, and claiming a handover that is not there
                // would send them to acknowledge nothing.
                None => BumpOutcome::VersionMismatch {
                    current: Some(current),
                },
            });
        };

        Ok(BumpOutcome::Issued(StoredBump {
            generation: row.get(0),
            edge_placement: row.get(1),
            superseded_generation: row.get(2),
            region_country: row.get(3),
            region_label: row.get(4),
        }))
    }

    /// Settles a handover by hand: clears `superseded_generation` when it holds `superseded`.
    ///
    /// One conditional statement, then at most one probe to phrase the refusal — the same shape as
    /// the bump, and for the same reason: the `WHERE` is the decision and cannot race, while the
    /// number in the message is advisory.
    ///
    /// There is no `If-Match` here and none is needed. Any bump necessarily *changes*
    /// `superseded_generation` — it writes the generation being displaced, which by construction is
    /// not the one already there — so a concurrent bump makes this `WHERE` fail on its own. A second
    /// precondition would refuse exactly the same requests while implying the two could disagree.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn settle_handover(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        superseded: i64,
    ) -> Result<StoredSettle, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let settled = connection
            .query_opt(
                "UPDATE store_lease SET superseded_generation = NULL \
                 WHERE tenant_id = $1 AND store_id = $2 \
                   AND superseded_generation = $3::bigint \
                 RETURNING generation",
                &[&tenant.to_string(), &store_id.to_string(), &superseded],
            )
            .await
            .map_err(unavailable)?;
        if settled.is_some() {
            return Ok(StoredSettle::Settled);
        }
        let probe = connection
            .query_opt(
                "SELECT superseded_generation FROM store_lease \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(match probe {
            None => StoredSettle::NoLease,
            Some(row) => StoredSettle::NotSuperseded {
                current: row.get(0),
            },
        })
    }

    /// Records that a settled handover's outgoing machine is no longer needed.
    ///
    /// Both preconditions live in the `WHERE`, so two admins retiring the same handover at once
    /// produce one recorded decision and one refusal naming the person who made it — rather than the
    /// second silently replacing the first in a row whose entire job is to hold the first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn retire_handover(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        retired_at_ms: i64,
        retired_by: &str,
    ) -> Result<StoredRetire, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let retired = connection
            .query_opt(
                // `generation > 0` is the third precondition, and it is not defensive: generation 0
                // is a store's *first* lease, which supersedes nobody (the insert in
                // `bump_store_lease` writes `superseded_generation = NULL` for exactly that reason).
                // Without it the other two conditions both hold on a brand-new store, and an
                // operator could record that its only machine — the one selling in the shop — was no
                // longer needed. The console cannot reach that (a gen-0 store derives no handover
                // state, so it offers no button), but the route is reachable directly, and a
                // precondition an operator cannot see is exactly the kind that has to live in the
                // `WHERE`.
                "UPDATE store_lease SET retired_at = $3, retired_by = $4 \
                 WHERE tenant_id = $1 AND store_id = $2 \
                   AND generation > 0 \
                   AND superseded_generation IS NULL \
                   AND retired_at IS NULL \
                 RETURNING generation",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &retired_at_ms,
                    &retired_by,
                ],
            )
            .await
            .map_err(unavailable)?;
        if retired.is_some() {
            return Ok(StoredRetire::Retired);
        }
        let probe = connection
            .query_opt(
                "SELECT superseded_generation, retired_at, retired_by, generation \
                 FROM store_lease WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        let Some(probe) = probe else {
            return Ok(StoredRetire::NoLease);
        };
        // The handover first, in the statement's own order: a person told "already retired" would
        // stop looking, and an in-flight handover is the fact that actually needs their attention.
        if let Some(superseded) = probe.get::<_, Option<i64>>(0) {
            return Ok(StoredRetire::Undrained { superseded });
        }
        // Before the generation check, deliberately: a row retired under the behaviour that
        // predates the `generation > 0` precondition holds a decision a real person made, and
        // reporting who and when is more use than telling them the row should not exist.
        if let (Some(at), Some(by)) = (probe.get(1), probe.get::<_, Option<String>>(2)) {
            return Ok(StoredRetire::AlreadyRetired { at, by });
        }
        if probe.get::<_, i64>(3) == 0 {
            return Ok(StoredRetire::NeverSuperseded);
        }
        // Every condition holds now, so the refusal was a race another admin has since resolved —
        // or resolved and then a bump reopened. Either way the caller re-reads, which is the right
        // next move, and this reports the state honestly rather than inventing a decider.
        Ok(StoredRetire::Raced)
    }

    /// The store's authoritative lease generation, or `None` if it has never been issued one.
    ///
    /// `None` is not `0`: a store that has never been issued a lease is one no box can be superseded
    /// against, and a store on generation `0` has exactly one machine that may be.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn store_lease(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<i64>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT generation FROM store_lease WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Advances a store's `last_seen_at` from a heartbeat ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md)),
    /// leaving `config_version_held` and `last_config_pull_at` untouched on an existing row (a fresh
    /// row gets them `NULL`, since a heartbeat carries no config-pull facts). `seen_at_ms` is Unix
    /// milliseconds.
    ///
    /// `outbox_depth` is the store's own publish backlog if it reported one, stamped with
    /// `seen_at_ms` as `outbox_reported_at`. A `None` — an older edge, or one that could not read its
    /// log — leaves both columns exactly as they were: "did not say" is not "zero", and overwriting a
    /// real backlog with a fabricated zero would read as a store that had caught up.
    ///
    /// `lease_generation` is the generation the box says it holds
    /// ([ADR-0108](../../../../docs/adr/0108-the-lease-generation-is-authority.md)), under the same
    /// rule and for a sharper reason: generation `0` is a store's *first* real lease, so writing a
    /// zero for a box that said nothing would report a replaced machine as current.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_heartbeat(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        seen_at_ms: i64,
        outbox_depth: Option<i64>,
        lease_generation: Option<i64>,
        print_agents: Option<String>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        // `print_agents` is a JSON array bound as text and cast, the same shape `order_queue` uses,
        // so no serde ToSql mapping is pulled into this crate. It COALESCEs like its neighbours, and
        // the distinction that makes that correct is between `NULL` and `'[]'`: a beat that omitted
        // the key leaves the stored list alone, and a beat carrying an *empty* list replaces it —
        // which is what a manager releasing the last terminal produces, and a console must stop
        // showing an agent nobody is bound to any more (ADR-0112).
        //
        // Two tables, one statement, one transaction. The liveness upsert is a data-modifying CTE
        // and the lease clear is the outer statement, so a heartbeat cannot record a drained store
        // and fail to release its handover, or the reverse.
        //
        // **The clear reads `$4` and `$5` — this beat's numbers — and never `store_liveness`.**
        // That is the whole correctness argument. The stored row COALESCEs depth and generation
        // *independently*, each with its own instant, so the pair it holds can come from two
        // different beats: generation N+1 recorded today beside a zero depth recorded last week
        // under generation N. A clear keyed on the stored row would fire on that pair and declare a
        // handover settled while the old machine still holds a night's trading — precisely the
        // failure ADR-0110 created the column to prevent. The request's two numbers arrived in one
        // message from one machine, so they are the only pair that means what the rule needs.
        //
        // Equality on the generation, not `>=`: the column names a specific machine's unsent
        // events, and a *different* generation reporting empty says nothing about that one. A beat
        // that omits either field leaves both `NULL`, the predicate is unknown, and nothing is
        // cleared — which is ADR-0110's \"an older edge that sends neither simply is not yet
        // provably settled\", falling out of SQL's three-valued logic rather than needing a branch.
        connection
            .execute(
                "WITH liveness AS ( \
                     INSERT INTO store_liveness \
                     (tenant_id, store_id, last_seen_at, outbox_depth, outbox_reported_at, \
                      lease_generation, lease_reported_at, print_agents, print_agents_reported_at) \
                     VALUES ($1, $2, $3, $4, \
                             CASE WHEN $4::bigint IS NULL THEN NULL ELSE $3::bigint END, \
                             $5, CASE WHEN $5::bigint IS NULL THEN NULL ELSE $3::bigint END, \
                             $6::text::jsonb, \
                             CASE WHEN $6::text IS NULL THEN NULL ELSE $3::bigint END) \
                     ON CONFLICT (tenant_id, store_id) DO UPDATE SET \
                     last_seen_at = EXCLUDED.last_seen_at, \
                     outbox_depth = COALESCE(EXCLUDED.outbox_depth, store_liveness.outbox_depth), \
                     outbox_reported_at = \
                         COALESCE(EXCLUDED.outbox_reported_at, store_liveness.outbox_reported_at), \
                     lease_generation = \
                         COALESCE(EXCLUDED.lease_generation, store_liveness.lease_generation), \
                     lease_reported_at = \
                         COALESCE(EXCLUDED.lease_reported_at, store_liveness.lease_reported_at), \
                     print_agents = \
                         COALESCE(EXCLUDED.print_agents, store_liveness.print_agents), \
                     print_agents_reported_at = COALESCE( \
                         EXCLUDED.print_agents_reported_at, store_liveness.print_agents_reported_at) \
                 ) \
                 UPDATE store_lease SET superseded_generation = NULL \
                 WHERE tenant_id = $1 AND store_id = $2 \
                   AND superseded_generation IS NOT NULL \
                   AND superseded_generation = $5::bigint \
                   AND $4::bigint = 0",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &seen_at_ms,
                    &outbox_depth,
                    &lease_generation,
                    &print_agents,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
