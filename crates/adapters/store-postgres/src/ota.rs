// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The OTA release registry over PostgreSQL ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md),
//! roadmap-v3 slice R2).
//!
//! Rows only. The artifact bytes live in the object store — a 30 MB binary per release per target in
//! the transactional database would ride along in every WAL archive, for data that is immutable and
//! content-addressable. What is here is the small record that says a release exists for a target, and
//! how big and what digest its bytes are; `pos-cloud` implements its `ReleaseStore` seam over this
//! type and derives the blob keys from the tag and target rather than storing them.
//!
//! The immutability rule is **not** in this SQL. `pos_cloud::ota::admit_artifact` owns it, and the
//! seam consults it with whatever digest [`Self::stored_digest`] returns, so the registry cannot drift
//! from the rule the tests pin. This adapter provides the read that feeds that decision and the
//! insert that follows it.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One recorded release artifact, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifactRow {
    /// The release tag the workflow cut.
    pub release: String,
    /// The Rust target triple the binary was compiled for.
    pub target: String,
    /// The executable's size in bytes.
    pub size_bytes: i64,
    /// Lowercase hex SHA-256 of the executable — an integrity check, never a trust boundary.
    pub sha256: String,
    /// When the artifact was recorded, Unix ms.
    pub recorded_at: i64,
}

/// The release registry over a shared pool. Built by
/// [`PostgresStore::releases`](crate::PostgresStore::releases).
#[derive(Clone, Debug)]
pub struct PostgresReleases {
    pool: Pool,
}

impl PostgresReleases {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The digest already recorded for `(release, target)`, or `None` if the pair is new.
    ///
    /// The input to the immutability decision: `None` admits a new artifact, an equal digest makes
    /// the upload a no-op, and a different one is refused.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn stored_digest(
        &self,
        release: &str,
        target: &str,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT sha256 FROM ota_releases WHERE release = $1 AND target = $2",
                &[&release, &target],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Inserts one artifact.
    ///
    /// `ON CONFLICT DO NOTHING` rather than an upsert: by the time this runs the caller has already
    /// admitted the write, and a row that appeared in between is either the identical artifact (so
    /// there is nothing to write) or one a concurrent uploader admitted — and in neither case may an
    /// insert silently redefine a version's bytes.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn insert_artifact(&self, artifact: &ReleaseArtifactRow) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO ota_releases (release, target, size_bytes, sha256, recorded_at) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (release, target) DO NOTHING",
                &[
                    &artifact.release,
                    &artifact.target,
                    &artifact.size_bytes,
                    &artifact.sha256,
                    &artifact.recorded_at,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// The artifact recorded for `(release, target)`, or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn find_artifact(
        &self,
        release: &str,
        target: &str,
    ) -> Result<Option<ReleaseArtifactRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT release, target, size_bytes, sha256, recorded_at FROM ota_releases \
                 WHERE release = $1 AND target = $2",
                &[&release, &target],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| ReleaseArtifactRow {
            release: row.get(0),
            target: row.get(1),
            size_bytes: row.get(2),
            sha256: row.get(3),
            recorded_at: row.get(4),
        }))
    }

    /// Every artifact recorded for `release`, ordered by target so a listing is stable.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_artifacts(
        &self,
        release: &str,
    ) -> Result<Vec<ReleaseArtifactRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT release, target, size_bytes, sha256, recorded_at FROM ota_releases \
                 WHERE release = $1 ORDER BY target ASC",
                &[&release],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| ReleaseArtifactRow {
                release: row.get(0),
                target: row.get(1),
                size_bytes: row.get(2),
                sha256: row.get(3),
                recorded_at: row.get(4),
            })
            .collect())
    }
}
