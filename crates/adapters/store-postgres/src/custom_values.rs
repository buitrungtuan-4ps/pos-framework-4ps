// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's custom-value table over PostgreSQL
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decision 3, migration 0092).
//!
//! The adapter keeps only the SQL. `pos-cloud-reports` folds the events and implements its
//! `CustomValueStore` seam over this type, handing it rows as plain text and JSON, so no
//! reporting type leaks into the adapter. A page of writes and the store's cursor commit in one
//! transaction, so an event is folded exactly once.

use deadpool_postgres::Pool;

use pos_ports::PortError;
use pos_proto::ids::{EventId, StoreId, TenantId};

use crate::store::{pool_unavailable, unavailable};

/// One write to the table, in the order a page's events made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CustomValueWrite {
    /// Insert or replace the row for an object and field.
    Upsert {
        /// The object's or record's id.
        object_id: String,
        /// The field's id.
        custom_field_id: String,
        /// The existing object's kind, for a field on an existing object.
        object: Option<String>,
        /// The object type, for a field of a record.
        object_type_id: Option<String>,
        /// The typed value, as JSON.
        value_json: String,
        /// The object's personal record, if it has one.
        subject_id: Option<String>,
        /// The package version the value follows.
        package_version_id: String,
        /// When the value was set, in Unix milliseconds.
        update_time_ms: i64,
    },
    /// Delete the row for an object and field: its value was cleared.
    Delete {
        /// The object's or record's id.
        object_id: String,
        /// The field's id.
        custom_field_id: String,
    },
    /// The store's events name this subject (migration 0093): a row if it has none yet.
    Subject {
        /// The subject's id.
        subject_id: String,
        /// When the store first named it, in Unix milliseconds.
        first_time_ms: i64,
    },
    /// The store confirmed it masked this subject: its row's `masked_time`, if not set already.
    SubjectMasked {
        /// The subject's id.
        subject_id: String,
        /// When the store masked it, in Unix milliseconds.
        masked_time_ms: i64,
    },
}

/// The most stores [`PostgresCustomValues::subject_holders`] returns for one subject.
pub const SUBJECT_HOLDERS_MAX: i64 = 64;

/// The custom-value table over a shared pool. Built by
/// [`PostgresStore::custom_values`](crate::PostgresStore::custom_values).
#[derive(Clone, Debug)]
pub struct PostgresCustomValues {
    pool: Pool,
}

impl PostgresCustomValues {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The last event folded for a store, as its id's text, or `None` before the first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn cursor(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT event_id FROM custom_value_cursors WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Applies `writes` in order and moves the store's cursor to `cursor`, in one transaction.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or a statement fails; nothing
    /// is then written.
    pub async fn apply(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        writes: &[CustomValueWrite],
        cursor: EventId,
    ) -> Result<(), PortError> {
        let tenant = tenant.to_string();
        let store = store_id.to_string();
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;
        for write in writes {
            match write {
                CustomValueWrite::Upsert {
                    object_id,
                    custom_field_id,
                    object,
                    object_type_id,
                    value_json,
                    subject_id,
                    package_version_id,
                    update_time_ms,
                } => {
                    transaction
                        .execute(
                            "INSERT INTO custom_values (tenant_id, store_id, object_id, \
                             custom_field_id, object, object_type_id, value, subject_id, \
                             package_version_id, update_time) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb, $8, $9, \
                             to_timestamp($10::bigint::double precision / 1000.0)) \
                             ON CONFLICT (tenant_id, store_id, object_id, custom_field_id) \
                             DO UPDATE SET object = EXCLUDED.object, \
                             object_type_id = EXCLUDED.object_type_id, value = EXCLUDED.value, \
                             subject_id = EXCLUDED.subject_id, \
                             package_version_id = EXCLUDED.package_version_id, \
                             update_time = EXCLUDED.update_time",
                            &[
                                &tenant,
                                &store,
                                object_id,
                                custom_field_id,
                                object,
                                object_type_id,
                                value_json,
                                subject_id,
                                package_version_id,
                                update_time_ms,
                            ],
                        )
                        .await
                        .map_err(unavailable)?;
                }
                CustomValueWrite::Subject { .. } | CustomValueWrite::SubjectMasked { .. } => {
                    write_subject(&transaction, &tenant, &store, write).await?;
                }
                CustomValueWrite::Delete {
                    object_id,
                    custom_field_id,
                } => {
                    transaction
                        .execute(
                            "DELETE FROM custom_values WHERE tenant_id = $1 AND store_id = $2 \
                             AND object_id = $3 AND custom_field_id = $4",
                            &[&tenant, &store, object_id, custom_field_id],
                        )
                        .await
                        .map_err(unavailable)?;
                }
            }
        }
        transaction
            .execute(
                "INSERT INTO custom_value_cursors (tenant_id, store_id, event_id) \
                 VALUES ($1, $2, $3) ON CONFLICT (tenant_id, store_id) \
                 DO UPDATE SET event_id = EXCLUDED.event_id, update_time = now()",
                &[&tenant, &store, &cursor.to_string()],
            )
            .await
            .map_err(unavailable)?;
        transaction.commit().await.map_err(unavailable)
    }

    /// The stores whose events name `subject_id`, and the store the cloud kept it for (a delivery
    /// order's recipient, ADR-0215), each with when it confirmed masking it in Unix milliseconds,
    /// or `None` while it has not. At most [`SUBJECT_HOLDERS_MAX`], by store id.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn subject_holders(
        &self,
        tenant: TenantId,
        subject_id: &str,
    ) -> Result<Vec<(String, Option<i64>)>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT store_id, (extract(epoch FROM masked_time) * 1000)::bigint \
                 FROM custom_subjects WHERE tenant_id = $1 AND subject_id = $2 \
                 UNION \
                 SELECT kept.store_id, NULL::bigint FROM subjects kept \
                 WHERE kept.tenant_id = $1 AND kept.subject_id = $2 \
                 AND kept.store_id IS NOT NULL AND NOT EXISTS ( \
                     SELECT 1 FROM custom_subjects held WHERE held.tenant_id = $1 \
                     AND held.store_id = kept.store_id AND held.subject_id = $2) \
                 ORDER BY store_id LIMIT $3",
                &[&tenant.to_string(), &subject_id, &SUBJECT_HOLDERS_MAX],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
    }

    /// Which of `subject_ids` one store has confirmed masking.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn masked_among(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        subject_ids: &[String],
    ) -> Result<Vec<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT subject_id FROM custom_subjects WHERE tenant_id = $1 AND store_id = $2 \
                 AND subject_id = ANY($3) AND masked_time IS NOT NULL",
                &[&tenant.to_string(), &store_id.to_string(), &subject_ids],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Counts the objects of one store holding each value of `custom_field_id`, the most first, at
    /// most `limit` groups.
    ///
    /// `object` (an existing object's kind token) or `object_type_id` names what is counted; a
    /// `filter` keeps only the objects whose filter field's value contains the given JSON. Returns
    /// each group's value as JSON text and its count. It reads no `subject_id`: a report counts
    /// values, never people.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn summary(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        scope: &SummaryScope,
        limit: i64,
    ) -> Result<Vec<(String, i64)>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT v.value::text, count(*) FROM custom_values v \
                 WHERE v.tenant_id = $1 AND v.store_id = $2 AND v.custom_field_id = $3 \
                 AND (v.object = $4 OR v.object_type_id = $5) \
                 AND ($6::text IS NULL OR EXISTS (SELECT 1 FROM custom_values f \
                     WHERE f.tenant_id = v.tenant_id AND f.store_id = v.store_id \
                     AND f.object_id = v.object_id AND f.custom_field_id = $6 \
                     AND f.value @> $7::text::jsonb)) \
                 GROUP BY v.value ORDER BY count(*) DESC, v.value::text LIMIT $8",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &scope.custom_field_id,
                    &scope.object,
                    &scope.object_type_id,
                    &scope.filter_field_id,
                    &scope.filter_json,
                    &limit,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
    }
}

/// One write to `custom_subjects` (migration 0093), inside the page's transaction.
async fn write_subject(
    transaction: &deadpool_postgres::Transaction<'_>,
    tenant: &String,
    store: &String,
    write: &CustomValueWrite,
) -> Result<(), PortError> {
    match write {
        CustomValueWrite::Subject {
            subject_id,
            first_time_ms,
        } => {
            transaction
                .execute(
                    "INSERT INTO custom_subjects (tenant_id, store_id, subject_id, \
                     first_time) VALUES ($1, $2, $3, \
                     to_timestamp($4::bigint::double precision / 1000.0)) \
                     ON CONFLICT (tenant_id, store_id, subject_id) DO NOTHING",
                    &[tenant, store, subject_id, first_time_ms],
                )
                .await
                .map_err(unavailable)?;
        }
        CustomValueWrite::SubjectMasked {
            subject_id,
            masked_time_ms,
        } => {
            // An update, never an insert: a confirmation for a subject this store's events
            // never named makes no row, so it cannot read as one the store held.
            transaction
                .execute(
                    "UPDATE custom_subjects SET masked_time = COALESCE(masked_time, \
                     to_timestamp($4::bigint::double precision / 1000.0)) \
                     WHERE tenant_id = $1 AND store_id = $2 AND subject_id = $3",
                    &[tenant, store, subject_id, masked_time_ms],
                )
                .await
                .map_err(unavailable)?;
            // The one insert: a subject the cloud kept for this very store (a delivery order's
            // recipient, migration 0101, ADR-0215), which the store holds a copy of by design.
            transaction
                .execute(
                    "INSERT INTO custom_subjects (tenant_id, store_id, subject_id, first_time, \
                     masked_time) SELECT $1, $2, $3, \
                     to_timestamp($4::bigint::double precision / 1000.0), \
                     to_timestamp($4::bigint::double precision / 1000.0) \
                     WHERE EXISTS (SELECT 1 FROM subjects WHERE tenant_id = $1 \
                     AND store_id = $2 AND subject_id = $3) \
                     ON CONFLICT (tenant_id, store_id, subject_id) DO NOTHING",
                    &[tenant, store, subject_id, masked_time_ms],
                )
                .await
                .map_err(unavailable)?;
        }
        CustomValueWrite::Upsert { .. } | CustomValueWrite::Delete { .. } => {}
    }
    Ok(())
}

/// What a summary counts and groups by, as the table holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryScope {
    /// The field the objects are grouped by.
    pub custom_field_id: String,
    /// The existing object's kind, when the summary counts existing objects.
    pub object: Option<String>,
    /// The object type, when the summary counts records.
    pub object_type_id: Option<String>,
    /// The field a filter reads, if any.
    pub filter_field_id: Option<String>,
    /// The JSON the filter field's value must contain, such as `{"boolean":true}`.
    pub filter_json: Option<String>,
}
