// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Media renditions in Postgres `bytea` (Track M5, [ADR-0075](../../../docs/adr/0075-media-and-file-rail.md)).
//!
//! One row per uploaded image (`media_assets`, migration 0030): a minted `MediaId`, the content type,
//! and the two JPEG renditions the ADR-0042 pipeline produced (`thumbnail`, `detail`) as `bytea`. This
//! adapter keeps only the SQL and returns plain rows/bytes; `pos-cloud` implements its `MediaStore`
//! seam over this type. Tenant scoping is an explicit `WHERE tenant_id = $1` (the cloud connects as the
//! trusted pool owner, which bypasses RLS; the migration's policy is the second line), exactly as the
//! catalog and tax-rate adapters do. Media is immutable — there is no update path, only insert, a
//! single-rendition read, a summary list, and delete.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One media asset as listed — its identity and size, never its bytes (a listing must not ship the
/// renditions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaAssetRow {
    /// The media id (a ULID string).
    pub media_id: String,
    /// The stored content type (`image/jpeg` today).
    pub content_type: String,
    /// The detail rendition's size in bytes, for a listing.
    pub detail_bytes: i32,
    /// When the asset was stored, epoch milliseconds.
    pub created_at_ms: i64,
}

/// The media store over a shared pool. Built by [`PostgresStore::media`](crate::PostgresStore::media).
#[derive(Clone, Debug)]
pub struct PostgresMedia {
    pool: Pool,
}

impl PostgresMedia {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts one asset's two renditions.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert(
        &self,
        media_id: &str,
        tenant_id: &str,
        content_type: &str,
        thumbnail: &[u8],
        detail: &[u8],
        detail_bytes: i32,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO media_assets \
                 (media_id, tenant_id, content_type, thumbnail, detail, detail_bytes) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &media_id,
                    &tenant_id,
                    &content_type,
                    &thumbnail,
                    &detail,
                    &detail_bytes,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's assets, newest first, without their bytes.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_summaries(&self, tenant_id: &str) -> Result<Vec<MediaAssetRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT media_id, content_type, detail_bytes, \
                 (EXTRACT(EPOCH FROM created_at) * 1000)::bigint \
                 FROM media_assets WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| MediaAssetRow {
                media_id: row.get(0),
                content_type: row.get(1),
                detail_bytes: row.get(2),
                created_at_ms: row.get(3),
            })
            .collect())
    }

    /// Reads one rendition's bytes (the detail when `detail` is true, else the thumbnail), or `None`
    /// if the tenant has no such asset. The column is a compile-time literal, never interpolated input.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_rendition(
        &self,
        tenant_id: &str,
        media_id: &str,
        detail: bool,
    ) -> Result<Option<Vec<u8>>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let sql = if detail {
            "SELECT detail FROM media_assets WHERE tenant_id = $1 AND media_id = $2"
        } else {
            "SELECT thumbnail FROM media_assets WHERE tenant_id = $1 AND media_id = $2"
        };
        let row = connection
            .query_opt(sql, &[&tenant_id, &media_id])
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Deletes one asset, returning whether a row was removed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn remove(&self, tenant_id: &str, media_id: &str) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let removed = connection
            .execute(
                "DELETE FROM media_assets WHERE tenant_id = $1 AND media_id = $2",
                &[&tenant_id, &media_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(removed == 1)
    }
}
