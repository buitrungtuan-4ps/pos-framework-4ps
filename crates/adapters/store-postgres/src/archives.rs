// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Store archives over PostgreSQL ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)).
//!
//! Rows only, for the reason `ota.rs` gives about release artifacts and more sharply: the archives
//! are whole store databases, and a store database in the transactional database would ride along
//! in every WAL segment. The sealed bytes live in the object store; what is here is the key each
//! store seals with and the small record of what it has shipped.
//!
//! **The key is stored wrapped and this adapter cannot unwrap it.** `pos_cloud::archive` holds
//! `ArchiveSecret` and does the wrapping; this type moves an opaque string. That is deliberate:
//! `deploy/backup.sh` ships a `pg_dump` of this database to the same off-box tier the archives sync
//! to ([ADR-0046](../../../docs/adr/0046-backups-and-restore.md)), so a usable key in a column
//! would put the key and the ciphertext it opens in one bucket (ADR-0124 Amendment 1). Keeping the
//! secret out of this crate makes that a property of the layering rather than a rule to remember.
//!
//! Tenant scoping is an explicit `WHERE tenant_id = $1` — the cloud connects as the trusted pool
//! owner, and the RLS policies in migration 0062 are the defence in depth for anything that does
//! not.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One archive a store has shipped, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreArchiveRow {
    /// The store the archive came from.
    pub store_id: String,
    /// When the store took the snapshot, Unix ms — its clock, not the cloud's.
    pub taken_at: i64,
    /// Where the sealed bytes are in the object store.
    pub object_key: String,
    /// How many bytes, sealed.
    pub size_bytes: i64,
    /// Lowercase hex SHA-256 of the sealed bytes as they arrived.
    pub sha256: String,
    /// When the cloud received it, Unix ms.
    pub received_at: i64,
}

/// The archive registry over a shared pool. Built by
/// [`PostgresStore::archives`](crate::PostgresStore::archives).
#[derive(Clone, Debug)]
pub struct PostgresArchives {
    pool: Pool,
}

impl PostgresArchives {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The wrapped key recorded for a store, or `None` if it has never asked for one.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn wrapped_key(
        &self,
        tenant_id: &str,
        store_id: &str,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT wrapped_key FROM store_archive_keys \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant_id, &store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Records `wrapped` **only if the store has no key**, and returns whichever key is current
    /// afterwards.
    ///
    /// One statement, so two tills asking in the same second cannot end up sealing under two
    /// different keys — one of which would then be the key to archives nobody can open. The
    /// `ON CONFLICT … DO UPDATE SET wrapped_key = store_archive_keys.wrapped_key` is a no-op write
    /// chosen for its `RETURNING`: `DO NOTHING` returns no row, which would leave the loser of the
    /// race unable to tell "somebody else won" from "the insert failed".
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn adopt_key(
        &self,
        tenant_id: &str,
        store_id: &str,
        wrapped: &str,
        minted_at: i64,
    ) -> Result<String, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "INSERT INTO store_archive_keys (tenant_id, store_id, wrapped_key, minted_at) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (tenant_id, store_id) \
                 DO UPDATE SET wrapped_key = store_archive_keys.wrapped_key \
                 RETURNING wrapped_key",
                &[&tenant_id, &store_id, &wrapped, &minted_at],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    /// Records an archive that arrived, replacing any row for the same snapshot instant.
    ///
    /// Upsert, unlike the key above: a retried upload of the *same* snapshot is a retry, and the
    /// second attempt's object key and digest are the ones that are true.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_archive(
        &self,
        tenant_id: &str,
        archive: &StoreArchiveRow,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO store_archives \
                     (tenant_id, store_id, taken_at, object_key, size_bytes, sha256, received_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (tenant_id, store_id, taken_at) DO UPDATE SET \
                     object_key = EXCLUDED.object_key, \
                     size_bytes = EXCLUDED.size_bytes, \
                     sha256 = EXCLUDED.sha256, \
                     received_at = EXCLUDED.received_at",
                &[
                    &tenant_id,
                    &archive.store_id,
                    &archive.taken_at,
                    &archive.object_key,
                    &archive.size_bytes,
                    &archive.sha256,
                    &archive.received_at,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// A store's archives, newest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_archives(
        &self,
        tenant_id: &str,
        store_id: &str,
        limit: i64,
    ) -> Result<Vec<StoreArchiveRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT store_id, taken_at, object_key, size_bytes, sha256, received_at \
                 FROM store_archives \
                 WHERE tenant_id = $1 AND store_id = $2 \
                 ORDER BY taken_at DESC LIMIT $3",
                &[&tenant_id, &store_id, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .into_iter()
            .map(|row| StoreArchiveRow {
                store_id: row.get(0),
                taken_at: row.get(1),
                object_key: row.get(2),
                size_bytes: row.get(3),
                sha256: row.get(4),
                received_at: row.get(5),
            })
            .collect())
    }
}
