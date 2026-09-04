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

use crate::store::{pool_unavailable, unavailable, window_total};

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
                 FROM media_assets WHERE tenant_id = $1 \
                 ORDER BY created_at DESC, media_id DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(media_asset_row).collect())
    }

    /// One page of a tenant's assets, newest first, with the size of the whole library.
    ///
    /// `count(*) OVER()` rides on the windowed `SELECT` rather than running separately: one round
    /// trip, one snapshot, so the count cannot disagree with the page it labels. An empty window
    /// carries no count at all, which [`window_total`] answers with a second query rather than a
    /// misleading zero.
    ///
    /// The `ORDER BY` is total — `created_at DESC, media_id DESC` — because `created_at` alone is
    /// not. It defaults to `now()`, which is transaction time, so a batch of uploads shares one
    /// value exactly and a window over the tie could repeat or skip a row across pages (ADR-0098
    /// decision 9). `media_assets_by_tenant_newest` (migration 0041) carries that whole order, so
    /// the index walk *is* the sort and `LIMIT` stops the scan.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_summaries_page(
        &self,
        tenant_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<MediaAssetRow>, i64), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT media_id, content_type, detail_bytes, \
                 (EXTRACT(EPOCH FROM created_at) * 1000)::bigint, \
                 count(*) OVER() \
                 FROM media_assets WHERE tenant_id = $1 \
                 ORDER BY created_at DESC, media_id DESC \
                 LIMIT $2 OFFSET $3",
                &[&tenant_id, &limit, &offset],
            )
            .await
            .map_err(unavailable)?;
        let total = window_total(
            &connection,
            &rows,
            4,
            "SELECT count(*) FROM media_assets WHERE tenant_id = $1",
            &[&tenant_id],
        )
        .await?;
        Ok((rows.iter().map(media_asset_row).collect(), total))
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

/// Reads one media summary out of a query result.
///
/// Shared by the paged and unpaged reads so their column order cannot drift: the paged query appends
/// a fifth column and this reads the first four, so a column added at the *front* of either breaks
/// both together rather than silently mismatching one.
fn media_asset_row(row: &tokio_postgres::Row) -> MediaAssetRow {
    MediaAssetRow {
        media_id: row.get(0),
        content_type: row.get(1),
        detail_bytes: row.get(2),
        created_at_ms: row.get(3),
    }
}
