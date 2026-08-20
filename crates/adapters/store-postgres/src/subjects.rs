// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The subject store over PostgreSQL — where personal data lives and is masked (P7, [ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).
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
                 WHERE masked_at IS NULL AND collected_at <= $1 \
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
            })
            .collect())
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
