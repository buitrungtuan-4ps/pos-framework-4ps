// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The device-proposal table over PostgreSQL (P7, [ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
//!
//! The discover→propose→admin-approves onboarding queue: a store proposes a discovered printer or KDS
//! and a super-admin resolves it. This adapter keeps only the SQL and returns plain rows; `pos-cloud`
//! implements its `DeviceProposalStore` seam over this type.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One proposal as listed — the durable facts plus its status. Never more than one store's worth of
/// rows at a time (the query filters by tenant, and optionally by store).
#[derive(Clone, Debug)]
pub struct DeviceProposalRow {
    /// The proposal id (a ULID string).
    pub id: String,
    /// The store that discovered the device.
    pub store_id: String,
    /// `printer` or `kds`.
    pub kind: String,
    /// The device's name.
    pub name: String,
    /// The device's network address.
    pub address: String,
    /// `usb`, `network` or `serial` — recorded at approval, `None` while pending.
    pub connection: Option<String>,
    /// The kitchen station this device serves — recorded at approval, `None` for the counter's
    /// receipt printer and while pending.
    pub station_id: Option<String>,
    /// `pending`, `approved`, or `rejected`.
    pub status: String,
}

/// The device-proposal store over a shared pool. Built by
/// [`PostgresStore::device_proposals`](crate::PostgresStore::device_proposals).
#[derive(Clone, Debug)]
pub struct PostgresDeviceProposals {
    pool: Pool,
}

impl PostgresDeviceProposals {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts a freshly-proposed device (status `pending`).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn create(
        &self,
        id: &str,
        tenant_id: &str,
        store_id: &str,
        kind: &str,
        name: &str,
        address: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO device_proposals (id, tenant_id, store_id, kind, name, address) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[&id, &tenant_id, &store_id, &kind, &name, &address],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's proposals in `status`, newest first — every store when `store_id` is `None`,
    /// or one store when `Some`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(
        &self,
        tenant_id: &str,
        store_id: Option<&str>,
        status: &str,
    ) -> Result<Vec<DeviceProposalRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT id, store_id, kind, name, address, connection, station_id, status \
                 FROM device_proposals \
                 WHERE tenant_id = $1 AND ($2::text IS NULL OR store_id = $2) AND status = $3 \
                 ORDER BY created_at DESC",
                &[&tenant_id, &store_id, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| DeviceProposalRow {
                id: row.get(0),
                store_id: row.get(1),
                kind: row.get(2),
                name: row.get(3),
                address: row.get(4),
                connection: row.get(5),
                station_id: row.get(6),
                status: row.get(7),
            })
            .collect())
    }

    /// Moves a **pending** proposal to `status` (`approved`/`rejected`) within `tenant_id`, stamping
    /// `resolved_at` and the two facts approval carries: how the device is attached, and the station
    /// it serves (ADR-0100). Returns whether a pending row was found and changed — so resolving an
    /// already-resolved or unknown id changes nothing.
    ///
    /// A rejection passes `None` for both, and they stay null: they describe a device the store will
    /// address, and a rejected one never will.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn mark(
        &self,
        tenant_id: &str,
        id: &str,
        status: &str,
        connection_kind: Option<&str>,
        station_id: Option<&str>,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE device_proposals \
                 SET status = $3, resolved_at = now(), connection = $4, station_id = $5 \
                 WHERE tenant_id = $1 AND id = $2 AND status = 'pending'",
                &[&tenant_id, &id, &status, &connection_kind, &station_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }
}
