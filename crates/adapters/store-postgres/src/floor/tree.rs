// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An area's writes, which keep a store's areas a tree
//! ([ADR-0211](../../../../../docs/adr/0211-an-area-may-sit-inside-another-area.md)).
//!
//! The rule (no cycle, at most eight levels, a live area's parent is live, no archiving with live
//! children) is `pos-cloud`'s and arrives as a `check` over the store's areas. What this module
//! owns is that the check reads the store as the write will find it: each write takes a
//! transaction-scoped advisory lock on its tenant and store first, so two writes to one store's
//! areas run one after the other, and the second reads what the first committed. Row locks alone
//! would not do, because they cannot hold back a row that does not exist yet.

use pos_ports::PortError;

use super::PostgresFloor;
use crate::store::{pool_unavailable, unavailable};

/// One area's place in its store's tree, as a write's `check` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AreaLinkRow {
    /// The area id (a ULID string).
    pub id: String,
    /// The area it sits inside, or `None` at the top.
    pub parent_area_id: Option<String>,
    /// `active` or `archived`.
    pub status: String,
}

/// An area's fields as a write sets them.
#[derive(Clone, Copy, Debug)]
pub struct AreaFields<'a> {
    /// The owning tenant.
    pub tenant_id: &'a str,
    /// The area id.
    pub id: &'a str,
    /// The name.
    pub name: &'a str,
    /// `active` or `archived`. An insert is always `active`.
    pub status: &'a str,
    /// The area it sits inside, or `None` at the top.
    pub parent_area_id: Option<&'a str>,
}

/// What an area write did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AreaWrite<R> {
    /// Written. Carries the row's new version.
    Written(String),
    /// The row is there, at a different version.
    VersionMismatch,
    /// No such row.
    NotFound,
    /// The check refused the store as the write would leave it. Nothing was written.
    Refused(R),
}

/// Takes the store's area lock for the rest of `transaction`.
async fn lock_store_areas(
    transaction: &tokio_postgres::Transaction<'_>,
    tenant_id: &str,
    store_id: &str,
) -> Result<(), PortError> {
    transaction
        .execute(
            "SELECT pg_advisory_xact_lock(hashtextextended('floor_areas/' || $1 || '/' || $2, 0))",
            &[&tenant_id, &store_id],
        )
        .await
        .map_err(unavailable)?;
    Ok(())
}

/// The store's areas, as the check reads them, inside the lock.
async fn store_links(
    transaction: &tokio_postgres::Transaction<'_>,
    tenant_id: &str,
    store_id: &str,
) -> Result<Vec<AreaLinkRow>, PortError> {
    let rows = transaction
        .query(
            "SELECT id, parent_area_id, status FROM floor_areas \
             WHERE tenant_id = $1 AND store_id = $2",
            &[&tenant_id, &store_id],
        )
        .await
        .map_err(unavailable)?;
    Ok(rows
        .iter()
        .map(|row| AreaLinkRow {
            id: row.get(0),
            parent_area_id: row.get(1),
            status: row.get(2),
        })
        .collect())
}

impl PostgresFloor {
    /// Inserts an area into `store_id`, if `check` passes over the store's areas as they stand.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_area<R>(
        &self,
        store_id: &str,
        area: AreaFields<'_>,
        check: impl FnOnce(&[AreaLinkRow]) -> Option<R> + Send,
    ) -> Result<AreaWrite<R>, PortError> {
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;
        lock_store_areas(&transaction, area.tenant_id, store_id).await?;
        let links = store_links(&transaction, area.tenant_id, store_id).await?;
        if let Some(refusal) = check(&links) {
            return Ok(AreaWrite::Refused(refusal));
        }
        let row = transaction
            .query_one(
                "INSERT INTO floor_areas (id, tenant_id, store_id, name, parent_area_id) \
                 VALUES ($1, $2, $3, $4, $5) RETURNING xmin::text",
                &[
                    &area.id,
                    &area.tenant_id,
                    &store_id,
                    &area.name,
                    &area.parent_area_id,
                ],
            )
            .await
            .map_err(unavailable)?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(AreaWrite::Written(row.get(0)))
    }

    /// Sets an area's name, status and parent, within its tenant, if the row is still at
    /// `expected` and `check` passes over its store's areas as they stand.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_area<R>(
        &self,
        area: AreaFields<'_>,
        expected: &str,
        check: impl FnOnce(&[AreaLinkRow]) -> Option<R> + Send,
    ) -> Result<AreaWrite<R>, PortError> {
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;
        let store = transaction
            .query_opt(
                "SELECT store_id FROM floor_areas WHERE tenant_id = $1 AND id = $2",
                &[&area.tenant_id, &area.id],
            )
            .await
            .map_err(unavailable)?;
        let Some(store) = store else {
            return Ok(AreaWrite::NotFound);
        };
        let store_id: String = store.get(0);
        lock_store_areas(&transaction, area.tenant_id, &store_id).await?;
        // Inside the lock: a version that moved is a mismatch before it is anything else, so a
        // caller re-reads rather than being told about a tree it has not seen.
        let current = transaction
            .query_opt(
                "SELECT 1 FROM floor_areas WHERE tenant_id = $1 AND id = $2 AND xmin::text = $3",
                &[&area.tenant_id, &area.id, &expected],
            )
            .await
            .map_err(unavailable)?;
        if current.is_none() {
            return Ok(AreaWrite::VersionMismatch);
        }
        let links = store_links(&transaction, area.tenant_id, &store_id).await?;
        if let Some(refusal) = check(&links) {
            return Ok(AreaWrite::Refused(refusal));
        }
        let row = transaction
            .query_one(
                "UPDATE floor_areas SET name = $3, status = $4, parent_area_id = $5, \
                 updated_at = now() WHERE tenant_id = $1 AND id = $2 RETURNING xmin::text",
                &[
                    &area.tenant_id,
                    &area.id,
                    &area.name,
                    &area.status,
                    &area.parent_area_id,
                ],
            )
            .await
            .map_err(unavailable)?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(AreaWrite::Written(row.get(0)))
    }
}
