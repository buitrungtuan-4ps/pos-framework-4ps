// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The background projector: keeps every store's materialised rollup current
//! ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
//!
//! [`project`] brings one store's rollup up to date; this is the loop
//! around it. On each tick it asks a [`StoreCatalog`] for the whole fleet's `(tenant, store)` pairs
//! and projects each, so the materialised rollup the `/v1` dashboard reads is never more than one
//! interval stale. Ingest — the NATS cursor or the reconciliation re-push — only writes the event
//! log; this is the single writer of the rollup table, which is what keeps the fold in one place.
//!
//! Robust by construction: one store's projection failing is logged and counted, not propagated, so
//! a single bad store never stalls the fleet; only a failure to *list* the fleet ends a tick, and
//! the next tick retries. Idempotent, because [`project`] advances a
//! per-store cursor and folds each event exactly once.

use core::future::Future;
use core::time::Duration;

use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{StoreId, TenantId};

use super::projection::{RollupStore, project};

/// How often the projector sweeps the fleet, by default. Frequent enough that a dashboard is never
/// meaningfully stale, cheap because a store with no new events folds nothing.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// Enumerates the `(tenant, store)` pairs whose rollups the projector maintains — the fleet.
///
/// The seam between the projector and the database, so the loop is tested without one.
/// `store-postgres` answers it from the distinct `(tenant_id, store_id)` of the event log.
pub trait StoreCatalog {
    /// Every `(tenant, store)` with events, across all tenants.
    fn active_stores(
        &self,
    ) -> impl Future<Output = Result<Vec<(TenantId, StoreId)>, PortError>> + Send;
}

/// What one fleet-wide projection pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FleetReport {
    /// How many stores were projected successfully.
    pub stores: u64,
    /// How many events were folded across the whole fleet this pass.
    pub folded: u64,
    /// How many stores' projections failed (logged and skipped, not fatal).
    pub failed: u64,
}

/// Projects every store in the fleet once.
///
/// A per-store failure is logged and counted in [`FleetReport::failed`], never propagated — one
/// store's unreachable rollup row must not stop the rest of the fleet being kept current.
///
/// # Errors
///
/// [`PortError`] only if the fleet itself cannot be listed; per-store failures are absorbed.
pub async fn project_fleet<E, R, Cat>(
    events: &E,
    rollups: &R,
    catalog: &Cat,
) -> Result<FleetReport, PortError>
where
    E: EventStore,
    R: RollupStore,
    Cat: StoreCatalog,
{
    let fleet = catalog.active_stores().await?;
    let mut report = FleetReport::default();
    for (tenant, store_id) in fleet {
        match project(events, rollups, tenant, store_id).await {
            Ok(pass) => {
                report.stores = report.stores.saturating_add(1);
                report.folded = report.folded.saturating_add(pass.folded);
            }
            Err(error) => {
                report.failed = report.failed.saturating_add(1);
                tracing::error!(
                    %tenant, %store_id, %error,
                    "projecting a store's rollup failed; skipping it this pass"
                );
            }
        }
    }
    Ok(report)
}

/// Runs the projector on `interval` until `shutdown` resolves.
///
/// A listing failure is logged and retried on the next tick rather than crashing the cloud — a
/// dashboard one interval stale is a far smaller problem than a cloud that will not stay up.
pub async fn run<E, R, Cat>(
    events: E,
    rollups: R,
    catalog: Cat,
    interval: Duration,
    shutdown: impl Future<Output = ()>,
) where
    E: EventStore,
    R: RollupStore,
    Cat: StoreCatalog,
{
    tokio::pin!(shutdown);
    loop {
        match project_fleet(&events, &rollups, &catalog).await {
            Ok(report) if report.folded > 0 || report.failed > 0 => {
                tracing::info!(
                    stores = report.stores,
                    folded = report.folded,
                    failed = report.failed,
                    "rollup projector pass"
                );
            }
            Ok(_) => tracing::debug!("rollup projector found no new events"),
            Err(error) => {
                tracing::error!(%error, "listing the fleet for projection failed; will retry");
            }
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("rollup projector shutting down");
                return;
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::{FleetReport, StoreCatalog, project_fleet};

    use pos_contract_tests::fixtures;
    use pos_fakes::FakeStore;
    use pos_ports::event_store::EventStore;
    use pos_ports::{PortError, Transactional as _, TxContext as _};
    use pos_proto::BusinessDate;
    use pos_proto::envelope::{EventEnvelope, RawPayload};
    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::ulid::Ulid;

    use crate::dashboard::projection::{RollupError, RollupStore, StoredRollups};

    fn store_id() -> StoreId {
        StoreId::new(Ulid::from_u128(0x0ADA))
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(0x7E11A))
    }

    fn dated(count: u32, year: i16, month: u8, day: u8) -> Vec<EventEnvelope<RawPayload>> {
        let date = BusinessDate::from_ymd(year, month, day).expect("a valid date");
        let mut events = fixtures::activations(store_id(), 1, count);
        for event in &mut events {
            event.business_date = date;
        }
        events
    }

    async fn append(store: &FakeStore, events: &[EventEnvelope<RawPayload>]) {
        let mut tx = store.begin().await.expect("begin");
        store.append(&mut tx, events).await.expect("append");
        tx.commit().await.expect("commit");
    }

    #[derive(Default)]
    struct FakeRollups {
        rows: Mutex<HashMap<(TenantId, StoreId), StoredRollups>>,
    }

    impl RollupStore for FakeRollups {
        async fn load(
            &self,
            tenant: TenantId,
            store: StoreId,
        ) -> Result<StoredRollups, RollupError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .get(&(tenant, store))
                .cloned()
                .unwrap_or_default())
        }

        async fn save(
            &self,
            tenant: TenantId,
            store: StoreId,
            rollups: &StoredRollups,
        ) -> Result<(), RollupError> {
            self.rows
                .lock()
                .expect("lock")
                .insert((tenant, store), rollups.clone());
            Ok(())
        }
    }

    /// A fixed fleet listing.
    struct Catalog(Vec<(TenantId, StoreId)>);

    impl StoreCatalog for Catalog {
        async fn active_stores(&self) -> Result<Vec<(TenantId, StoreId)>, PortError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn a_pass_folds_every_store_in_the_fleet_then_is_idempotent() {
        let events = FakeStore::new();
        append(&events, &dated(3, 2026, 3, 15)).await;
        let rollups = FakeRollups::default();
        let catalog = Catalog(vec![(tenant(), store_id())]);

        let first = project_fleet(&events, &rollups, &catalog)
            .await
            .expect("first pass");
        assert_eq!(
            first,
            FleetReport {
                stores: 1,
                folded: 3,
                failed: 0
            }
        );

        // The rollup is now populated for (tenant, store).
        let state = rollups.load(tenant(), store_id()).await.expect("load");
        let total: u64 = state.days.values().map(|day| day.total_events).sum();
        assert_eq!(total, 3);

        // A second pass with no new events folds nothing — the per-store cursor did not move.
        let second = project_fleet(&events, &rollups, &catalog)
            .await
            .expect("second pass");
        assert_eq!(second.folded, 0, "an idempotent pass folds nothing");
        assert_eq!(second.stores, 1);
    }

    #[tokio::test]
    async fn an_empty_fleet_projects_nothing() {
        let events = FakeStore::new();
        let rollups = FakeRollups::default();
        let catalog = Catalog(Vec::new());

        let report = project_fleet(&events, &rollups, &catalog)
            .await
            .expect("pass");
        assert_eq!(report, FleetReport::default(), "nothing to do");
    }
}
