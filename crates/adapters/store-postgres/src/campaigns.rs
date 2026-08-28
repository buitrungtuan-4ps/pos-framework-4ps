// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The campaign authoring table over PostgreSQL (Track M3, [ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)).
//!
//! One row per `(tenant, campaign)`; the `campaign` column is the whole authored campaign (the wire
//! `PublishedCampaign`) held as `jsonb` (`campaigns`, migration 0032). This adapter keeps only the SQL
//! and hands back the raw JSON text; `pos-cloud` implements its `CampaignStore` seam over this type and
//! does the `PublishedCampaign` (de)serialisation, so no cloud-domain type leaks into the adapter — the
//! same split the config-tree and rollup tables use. Tenant scoping is an explicit `WHERE tenant_id =
//! $1` (the cloud connects as the trusted pool owner, which bypasses RLS; the migration's policy is the
//! second line).

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One authored campaign as stored: its id (a ULID string) and the campaign document as JSON text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignRow {
    /// The campaign id (a ULID string), the row's key within its tenant.
    pub campaign_id: String,
    /// The whole `PublishedCampaign` as JSON text, as stored in the `campaign` jsonb column.
    pub campaign_json: String,
}

/// The campaign store over a shared pool. Built by
/// [`PostgresStore::campaigns`](crate::PostgresStore::campaigns).
#[derive(Clone, Debug)]
pub struct PostgresCampaigns {
    pool: Pool,
}

impl PostgresCampaigns {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Lists a tenant's campaigns, oldest first (id order is creation order for a ULID).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str) -> Result<Vec<CampaignRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT campaign_id, campaign::text FROM campaigns \
                 WHERE tenant_id = $1 ORDER BY campaign_id",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CampaignRow {
                campaign_id: row.get(0),
                campaign_json: row.get(1),
            })
            .collect())
    }

    /// One campaign by id, or `None` if the tenant has none with that id.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        tenant_id: &str,
        campaign_id: &str,
    ) -> Result<Option<CampaignRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT campaign_id, campaign::text FROM campaigns \
                 WHERE tenant_id = $1 AND campaign_id = $2",
                &[&tenant_id, &campaign_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| CampaignRow {
            campaign_id: row.get(0),
            campaign_json: row.get(1),
        }))
    }

    /// Creates a campaign, or replaces the one that already has its id.
    ///
    /// The `$3::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the same
    /// reason the config-tree and event tables cast their bound documents.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn upsert(
        &self,
        tenant_id: &str,
        campaign_id: &str,
        campaign_json: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO campaigns (tenant_id, campaign_id, campaign) \
                 VALUES ($1, $2, $3::text::jsonb) \
                 ON CONFLICT (tenant_id, campaign_id) \
                 DO UPDATE SET campaign = EXCLUDED.campaign, updated_at = now()",
                &[&tenant_id, &campaign_id, &campaign_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Removes a campaign by id. Removing one that does not exist is not an error.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete(&self, tenant_id: &str, campaign_id: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "DELETE FROM campaigns WHERE tenant_id = $1 AND campaign_id = $2",
                &[&tenant_id, &campaign_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
