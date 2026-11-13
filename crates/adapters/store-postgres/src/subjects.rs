// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The subject store over PostgreSQL — where personal data lives and is masked (P7, [ADR-0035](../../../../docs/adr/0035-retention-and-pii-masking.md)).
//!
//! The retention cron sweeps this table fleet-wide (as the trusted role), so these queries carry no
//! tenant: [`due_before`](PostgresSubjects::due_before) returns unmasked rows collected at or before a
//! cutoff, and [`save_masked`](PostgresSubjects::save_masked) writes the redacted fields back in place.
//! This adapter keeps only the SQL and returns plain types; `pos-cloud` implements its `SubjectStore`
//! seam over this type, converting rows to and from its `SubjectRecord`.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One subject row, as plain types — no cloud-domain type crosses the adapter boundary.
///
/// `pos-cloud` converts this into its `SubjectRecord`: `subject_id` parses to a `SubjectId`,
/// `collected_at_ms`/`masked_at_ms` are milliseconds since the Unix epoch, and `fields_json` is the
/// `{name, phone, …}` document.
#[derive(Clone, Debug)]
pub struct SubjectRow {
    /// The subject id (a ULID string).
    pub subject_id: String,
    /// When the personal data was collected, in milliseconds since the Unix epoch.
    pub collected_at_ms: i64,
    /// The personal fields as a JSON object (`{name: …, phone: …}`).
    pub fields_json: String,
    /// When the row was masked, in milliseconds since the Unix epoch, or `None` while it still holds
    /// personal data. The sweep's [`fetch_due`](PostgresSubjects::fetch_due) leaves this `None` (it
    /// returns only unmasked rows); [`fetch_one`](PostgresSubjects::fetch_one) reports the real stamp.
    pub masked_at_ms: Option<i64>,
}

/// A subject record kept for one store
/// ([ADR-0215](../../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 3): a delivery
/// order's recipient. Plain types, as [`SubjectRow`]; its `Debug` leaves `fields_json` out, so a
/// value cannot be printed by accident.
pub struct StoreSubjectRow<'a> {
    /// The subject id (a ULID string).
    pub subject_id: &'a str,
    /// The tenant (a ULID string).
    pub tenant_id: &'a str,
    /// The store it was collected for (a ULID string); only that store may read it.
    pub store_id: &'a str,
    /// When it was collected, in milliseconds since the Unix epoch.
    pub collected_at_ms: i64,
    /// When it is masked, in milliseconds since the Unix epoch, or `None` for the cloud's period.
    pub retain_until_ms: Option<i64>,
    /// The personal fields as a JSON object.
    pub fields_json: &'a str,
}

impl core::fmt::Debug for StoreSubjectRow<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("StoreSubjectRow")
            .field("subject_id", &self.subject_id)
            .field("store_id", &self.store_id)
            .finish_non_exhaustive()
    }
}

/// The subject store over a shared pool. Built by [`PostgresStore::subjects`](crate::PostgresStore::subjects).
#[derive(Clone, Debug)]
pub struct PostgresSubjects {
    pool: Pool,
}

impl PostgresSubjects {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The unmasked rows collected at or before `cutoff_ms`, at most `limit` of them, oldest first.
    /// A row that carries its own `retain_until` (migration 0101) is not one of them: it expires on
    /// its own clock, through [`fetch_expired`](Self::fetch_expired).
    ///
    /// Only `masked_at IS NULL` rows are returned, which is what makes the sweep idempotent: a row
    /// masked by [`mask`](Self::mask) is never handed back.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_due(
        &self,
        cutoff_ms: i64,
        limit: i64,
    ) -> Result<Vec<SubjectRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT subject_id, collected_at, fields::text FROM subjects \
                 WHERE masked_at IS NULL AND retain_until IS NULL AND collected_at <= $1 \
                 ORDER BY collected_at ASC LIMIT $2",
                &[&cutoff_ms, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| SubjectRow {
                subject_id: row.get(0),
                collected_at_ms: row.get(1),
                fields_json: row.get(2),
                // `fetch_due` selects only `masked_at IS NULL` rows.
                masked_at_ms: None,
            })
            .collect())
    }

    /// One subject row by id, scoped to the tenant that owns it — the read behind the PDPD/GDPR
    /// subject-request tooling ([ADR-0076](../../../../docs/adr/0076-subject-request-tooling.md)). Returns
    /// `None` when no such subject belongs to `tenant_id`, so an operator acting for one tenant can
    /// never read another's personal data even though `subject_id` is globally unique. Reports the real
    /// `masked_at` so the caller knows whether the row still holds data.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        subject_id: &str,
        tenant_id: &str,
    ) -> Result<Option<SubjectRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT subject_id, collected_at, fields::text, masked_at FROM subjects \
                 WHERE subject_id = $1 AND tenant_id = $2",
                &[&subject_id, &tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| SubjectRow {
            subject_id: row.get(0),
            collected_at_ms: row.get(1),
            fields_json: row.get(2),
            masked_at_ms: row.get(3),
        }))
    }

    /// The unmasked rows whose own `retain_until` is at or before `now_ms`, at most `limit` of them,
    /// soonest first (migration 0101,
    /// [ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
    /// decision 1). Like [`fetch_due`](Self::fetch_due), only `masked_at IS NULL` rows.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_expired(
        &self,
        now_ms: i64,
        limit: i64,
    ) -> Result<Vec<SubjectRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT subject_id, collected_at, fields::text FROM subjects \
                 WHERE masked_at IS NULL AND retain_until IS NOT NULL AND retain_until <= $1 \
                 ORDER BY retain_until ASC LIMIT $2",
                &[&now_ms, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| SubjectRow {
                subject_id: row.get(0),
                collected_at_ms: row.get(1),
                fields_json: row.get(2),
                masked_at_ms: None,
            })
            .collect())
    }

    /// Keeps a store's subject record: inserts it unless a row with `subject_id` is already there,
    /// in which case nothing changes (a vendor's retry carries the same id, and the first record
    /// stands). Returns whether a row was inserted.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn keep_for_store(&self, row: &StoreSubjectRow<'_>) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let inserted = connection
            .execute(
                "INSERT INTO subjects (subject_id, tenant_id, store_id, collected_at, \
                 retain_until, fields) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb) \
                 ON CONFLICT (subject_id) DO NOTHING",
                &[
                    &row.subject_id,
                    &row.tenant_id,
                    &row.store_id,
                    &row.collected_at_ms,
                    &row.retain_until_ms,
                    &row.fields_json,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(inserted == 1)
    }

    /// One unmasked subject row kept for `store_id` of `tenant_id`, with its `retain_until`, or
    /// `None` when there is none: another tenant's, another store's, a masked one, or an unknown
    /// id all read the same.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_for_store(
        &self,
        tenant_id: &str,
        store_id: &str,
        subject_id: &str,
    ) -> Result<Option<(SubjectRow, Option<i64>)>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT subject_id, collected_at, fields::text, retain_until FROM subjects \
                 WHERE subject_id = $1 AND tenant_id = $2 AND store_id = $3 \
                 AND masked_at IS NULL",
                &[&subject_id, &tenant_id, &store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| {
            (
                SubjectRow {
                    subject_id: row.get(0),
                    collected_at_ms: row.get(1),
                    fields_json: row.get(2),
                    masked_at_ms: None,
                },
                row.get(3),
            )
        }))
    }

    /// Writes one masked record back: the redacted `fields_json` and the `masked_at_ms` stamp, only
    /// while the row is still unmasked. Returns whether a row was updated.
    ///
    /// The `masked_at IS NULL` guard makes this idempotent at the database too: re-masking an
    /// already-masked row changes nothing.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn mask(
        &self,
        subject_id: &str,
        fields_json: &str,
        masked_at_ms: i64,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .execute(
                "UPDATE subjects SET fields = $2::text::jsonb, masked_at = $3 \
                 WHERE subject_id = $1 AND masked_at IS NULL",
                &[&subject_id, &fields_json, &masked_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(updated == 1)
    }
}
