// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's fee rules over PostgreSQL
//! ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 1).
//!
//! One row per `(tenant, scope, scope_id, fee_id)`; the `doc` column is the rule as authored, as
//! `jsonb` (`fee_rules`, migration 0075). This adapter keeps only the SQL and hands back the raw
//! JSON text; `pos-cloud` implements its `FeeRuleStore` seam over this type and does the
//! (de)serialisation, so no cloud-domain type reaches the adapter — the split `settings` uses.
//! Tenant scoping is an explicit `WHERE tenant_id = $1` (the cloud connects as the trusted pool
//! owner, which bypasses RLS; the migration's policy is the second line).
//!
//! `update_time` crosses as epoch milliseconds in both directions, as the reason codes' does: the
//! cloud's own clock writes it, so a test drives it, and one integer comes back with no timezone to
//! lose on the way.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// Where one rule sits: the scope it was written at, the id of that scope, and the rule's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeRuleSlot<'a> {
    /// The scope's wire token, such as `FEE_SCOPE_STORE`.
    pub scope: &'a str,
    /// The tenant, brand or store the rule was written for.
    pub scope_id: &'a str,
    /// The rule's stable id, a ULID string.
    pub fee_id: &'a str,
}

/// One stored rule: where it sits, the rule as JSON text, and who wrote it when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeRuleRow {
    /// The scope's wire token.
    pub scope: String,
    /// The tenant, brand or store the rule was written for.
    pub scope_id: String,
    /// The rule's id.
    pub fee_id: String,
    /// The rule as authored, as JSON text from the `doc` jsonb column.
    pub doc_json: String,
    /// When it was last written, in Unix milliseconds.
    pub update_time_ms: i64,
    /// The console admin who last wrote it, by id.
    pub updated_by: String,
}

/// A rule as a write gives it: the rule as JSON text, and who wrote it when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeRuleWrite<'a> {
    /// The rule as authored, as JSON text.
    pub doc_json: &'a str,
    /// When it was written, in Unix milliseconds, by the cloud's clock.
    pub update_time_ms: i64,
    /// The console admin who wrote it, by id.
    pub updated_by: &'a str,
}

/// The columns every read returns, in the order [`fee_rule_row`] reads them.
const FEE_RULE_COLUMNS: &str = "scope, scope_id, fee_id, doc::text, \
     (EXTRACT(EPOCH FROM update_time) * 1000)::bigint, updated_by";

/// The fee-rule store over a shared pool. Built by
/// [`PostgresStore::fee_rules`](crate::PostgresStore::fee_rules).
#[derive(Clone, Debug)]
pub struct PostgresFeeRules {
    pool: Pool,
}

impl PostgresFeeRules {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Every rule a tenant has written, in scope, scope-id and fee-id order. The order is by
    /// bytes (`COLLATE "C"`), whatever the database's locale, so it is the order the cloud's
    /// in-memory store lists in.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str) -> Result<Vec<FeeRuleRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {FEE_RULE_COLUMNS} FROM fee_rules WHERE tenant_id = $1 \
                     ORDER BY scope COLLATE \"C\", scope_id COLLATE \"C\", fee_id COLLATE \"C\""
                ),
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(fee_rule_row).collect())
    }

    /// Writes a rule, replacing the one already in its slot.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn upsert(
        &self,
        tenant_id: &str,
        slot: FeeRuleSlot<'_>,
        write: FeeRuleWrite<'_>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO fee_rules \
                     (tenant_id, scope, scope_id, fee_id, doc, update_time, updated_by) \
                 VALUES ($1, $2, $3, $4, $5::text::jsonb, \
                     to_timestamp($6::bigint::double precision / 1000.0), $7) \
                 ON CONFLICT (tenant_id, scope, scope_id, fee_id) DO UPDATE SET \
                     doc = EXCLUDED.doc, update_time = EXCLUDED.update_time, \
                     updated_by = EXCLUDED.updated_by",
                &[
                    &tenant_id,
                    &slot.scope,
                    &slot.scope_id,
                    &slot.fee_id,
                    &write.doc_json,
                    &write.update_time_ms,
                    &write.updated_by,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Removes the rule in one slot, and says whether there was one. Removing one that is not there
    /// is not an error.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete(&self, tenant_id: &str, slot: FeeRuleSlot<'_>) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let removed = connection
            .execute(
                "DELETE FROM fee_rules \
                 WHERE tenant_id = $1 AND scope = $2 AND scope_id = $3 AND fee_id = $4",
                &[&tenant_id, &slot.scope, &slot.scope_id, &slot.fee_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(removed > 0)
    }
}

/// One row of [`FEE_RULE_COLUMNS`].
fn fee_rule_row(row: &tokio_postgres::Row) -> FeeRuleRow {
    FeeRuleRow {
        scope: row.get(0),
        scope_id: row.get(1),
        fee_id: row.get(2),
        doc_json: row.get(3),
        update_time_ms: row.get(4),
        updated_by: row.get(5),
    }
}
