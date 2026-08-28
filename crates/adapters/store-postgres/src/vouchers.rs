// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voucher instances over PostgreSQL (Track M3, [ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)).
//!
//! One row per minted voucher code (`vouchers`, migration 0033). This adapter keeps only the SQL and
//! returns plain rows; `pos-cloud` implements its `VoucherStore` seam over this type. A batch inserts
//! in one transaction so a code collision (caught by the `(tenant_id, code)` unique constraint) fails
//! the whole batch rather than half-minting. Tenant scoping is an explicit `WHERE tenant_id = $1` (the
//! cloud connects as the trusted pool owner, which bypasses RLS; the migration's policy is the second
//! line).

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One voucher to insert: its id, campaign, and code (all strings on the wire to the database).
#[derive(Clone, Debug)]
pub struct NewVoucherRow<'a> {
    /// The voucher id (a ULID string).
    pub voucher_id: &'a str,
    /// The campaign id it redeems against (a ULID string).
    pub campaign_id: &'a str,
    /// The distributable code.
    pub code: &'a str,
}

/// One stored voucher row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoucherRow {
    /// The voucher id (a ULID string).
    pub voucher_id: String,
    /// The campaign id it redeems against.
    pub campaign_id: String,
    /// The distributable code.
    pub code: String,
    /// The lifecycle status token (`ACTIVE` / `REDEEMED` / `VOID`).
    pub status: String,
    /// When it was minted, Unix milliseconds.
    pub created_at_ms: i64,
}

/// The voucher store over a shared pool. Built by
/// [`PostgresStore::vouchers`](crate::PostgresStore::vouchers).
#[derive(Clone, Debug)]
pub struct PostgresVouchers {
    pool: Pool,
}

impl PostgresVouchers {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts a batch of vouchers for a tenant, atomically: the whole batch commits or none of it
    /// does, so a code collision (the `(tenant_id, code)` unique constraint) rolls the batch back
    /// rather than half-minting. Codes are ~60-bit random, so a collision is astronomically unlikely;
    /// if one occurs the caller retries and gets fresh codes.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or a write fails (a code collision
    /// surfaces this way, matching how the other adapters map a unique violation).
    pub async fn insert_batch(
        &self,
        tenant_id: &str,
        vouchers: &[NewVoucherRow<'_>],
    ) -> Result<(), PortError> {
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;
        for voucher in vouchers {
            transaction
                .execute(
                    "INSERT INTO vouchers (tenant_id, voucher_id, campaign_id, code) \
                     VALUES ($1, $2, $3, $4)",
                    &[
                        &tenant_id,
                        &voucher.voucher_id,
                        &voucher.campaign_id,
                        &voucher.code,
                    ],
                )
                .await
                .map_err(unavailable)?;
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(())
    }

    /// Lists a campaign's vouchers, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_by_campaign(
        &self,
        tenant_id: &str,
        campaign_id: &str,
    ) -> Result<Vec<VoucherRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT voucher_id, campaign_id, code, status, \
                 (extract(epoch from created_at) * 1000)::bigint \
                 FROM vouchers WHERE tenant_id = $1 AND campaign_id = $2 \
                 ORDER BY created_at DESC, voucher_id DESC",
                &[&tenant_id, &campaign_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| VoucherRow {
                voucher_id: row.get(0),
                campaign_id: row.get(1),
                code: row.get(2),
                status: row.get(3),
                created_at_ms: row.get(4),
            })
            .collect())
    }
}
