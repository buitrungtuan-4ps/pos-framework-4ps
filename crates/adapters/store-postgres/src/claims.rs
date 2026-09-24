// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The device-claim table over PostgreSQL
//! ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)).
//!
//! Each step is one guarded `UPDATE … WHERE status = …`, so a code binds once and a claim is
//! collected once even under a race: the loser matches no row and changes nothing. Collection inserts
//! the device credential in the same transaction that marks the claim collected, so the credential
//! can never belong to a slot other than the claim's, and the two commit together. Plain types in
//! and out; `pos-cloud` implements its `ClaimStore` seam over this.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// The slot a claim was bound to, as plain types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimSlotRow {
    /// The tenant the box belongs to.
    pub tenant_id: String,
    /// The store it was claimed for.
    pub store_id: String,
    /// The device slot it fills.
    pub device_id: String,
}

/// What binding a code came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimBindRow {
    /// Bound now.
    Bound,
    /// No claim has the code.
    Unknown,
    /// The claim's hour has passed.
    Expired,
    /// The claim was bound or collected before.
    AlreadyBound,
}

/// What a collection came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimCollectRow {
    /// Collected now, for this slot; the credential row is written.
    Collected(ClaimSlotRow),
    /// The claim is still waiting to be bound.
    Pending,
    /// The claim's hour has passed.
    Expired,
    /// No such claim, a wrong secret, or one already collected.
    Refused,
}

/// The claim store over a shared pool. Built by [`PostgresStore::claims`](crate::PostgresStore::claims).
#[derive(Clone, Debug)]
pub struct PostgresClaims {
    pool: Pool,
}

impl PostgresClaims {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Records a newly opened claim: its id, the hashes of its code and secret, and when it expires
    /// (Unix milliseconds).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails, including a
    /// code hash another claim already holds.
    pub async fn open(
        &self,
        claim_id: &str,
        user_code_hash: &[u8],
        secret_hash: &[u8],
        expires_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO device_claims (claim_id, user_code_hash, secret_hash, expires_at) \
                 VALUES ($1, $2, $3, $4)",
                &[&claim_id, &user_code_hash, &secret_hash, &expires_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Binds the pending, unexpired claim holding `user_code_hash` to a slot.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn bind(
        &self,
        user_code_hash: &[u8],
        slot: &ClaimSlotRow,
        now_ms: i64,
    ) -> Result<ClaimBindRow, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let bound = connection
            .execute(
                "UPDATE device_claims \
                 SET status = 'bound', tenant_id = $2, store_id = $3, device_id = $4, \
                     bound_at = now() \
                 WHERE user_code_hash = $1 AND status = 'pending' AND expires_at > $5",
                &[
                    &user_code_hash,
                    &slot.tenant_id,
                    &slot.store_id,
                    &slot.device_id,
                    &now_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        if bound == 1 {
            return Ok(ClaimBindRow::Bound);
        }
        // Nothing moved: say why, from the row as it stands.
        let row = connection
            .query_opt(
                "SELECT status, expires_at FROM device_claims WHERE user_code_hash = $1",
                &[&user_code_hash],
            )
            .await
            .map_err(unavailable)?;
        Ok(match row {
            None => ClaimBindRow::Unknown,
            Some(row) => {
                let status: String = row.get(0);
                let expires_at: i64 = row.get(1);
                if status != "pending" {
                    ClaimBindRow::AlreadyBound
                } else if expires_at <= now_ms {
                    ClaimBindRow::Expired
                } else {
                    // Pending and unexpired, yet not updated: a concurrent bind won between the two
                    // statements. It is bound now, by someone else.
                    ClaimBindRow::AlreadyBound
                }
            }
        })
    }

    /// Collects a bound claim: marks it collected and inserts the device credential for its slot, in
    /// one transaction. Anything else changes nothing and says why.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or a statement fails.
    pub async fn collect(
        &self,
        claim_id: &str,
        secret_hash: &[u8],
        credential_id: &str,
        credential_secret_hash: &[u8],
        now_ms: i64,
    ) -> Result<ClaimCollectRow, PortError> {
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;
        let collected = transaction
            .query_opt(
                "UPDATE device_claims \
                 SET status = 'collected', collected_at = now(), credential_id = $3 \
                 WHERE claim_id = $1 AND secret_hash = $2 AND status = 'bound' AND expires_at > $4 \
                 RETURNING tenant_id, store_id, device_id",
                &[&claim_id, &secret_hash, &credential_id, &now_ms],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = collected {
            let slot = ClaimSlotRow {
                tenant_id: row.get(0),
                store_id: row.get(1),
                device_id: row.get(2),
            };
            transaction
                .execute(
                    "INSERT INTO device_credentials (id, tenant_id, store_id, device_id, secret_hash) \
                     VALUES ($1, $2, $3, $4, $5)",
                    &[
                        &credential_id,
                        &slot.tenant_id,
                        &slot.store_id,
                        &slot.device_id,
                        &credential_secret_hash,
                    ],
                )
                .await
                .map_err(unavailable)?;
            transaction.commit().await.map_err(unavailable)?;
            return Ok(ClaimCollectRow::Collected(slot));
        }
        // Nothing moved. The transaction rolls back when dropped; read why from the row. A wrong
        // secret is told apart from nothing: it is `Refused`, like an unknown claim.
        let row = transaction
            .query_opt(
                "SELECT status, expires_at FROM device_claims WHERE claim_id = $1 AND secret_hash = $2",
                &[&claim_id, &secret_hash],
            )
            .await
            .map_err(unavailable)?;
        Ok(match row {
            None => ClaimCollectRow::Refused,
            Some(row) => {
                let status: String = row.get(0);
                let expires_at: i64 = row.get(1);
                match status.as_str() {
                    "collected" => ClaimCollectRow::Refused,
                    _ if expires_at <= now_ms => ClaimCollectRow::Expired,
                    _ => ClaimCollectRow::Pending,
                }
            }
        })
    }
}
