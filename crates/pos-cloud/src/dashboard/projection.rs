// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The materialised rollup: maintained incrementally, read cheaply
//! ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
//!
//! A dashboard cannot re-scan the whole event log on every view and still answer in the <10 ms the
//! P7 exit criterion asks for. So the rollup is **materialised**: [`project`] folds each new event
//! once, advancing a cursor, and stores the running per-day totals; [`dashboard`] then answers from
//! that stored rollup with **no event-log scan at all** — its signature does not even take an
//! `EventStore`, which is what makes "answers from rollups, not the log" a fact of the type, not a
//! promise. Because it folds with the *same* [`fold_event`] the from-log
//! computation uses, and visits each event exactly once via the cursor, the materialised rollup
//! equals what a full re-scan would produce — the property the tests assert. Resetting the stored
//! cursor and re-projecting rebuilds it from the log.

use core::future::Future;
use core::num::NonZeroU32;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_ports::PortError;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{EventId, StoreId, TenantId};

use crate::cloud::{DailyCash, DailyRevenue, DailyRollup, XzReport};

use super::rollup::{
    RollupWindow, fold_cash, fold_event, fold_revenue, render_revenue_window, render_window,
};

/// How many events one projection page reads and folds.
const PROJECT_PAGE: u32 = 512;

/// One store's materialised rollup: the folded days, and how far into the log they reflect.
///
/// (De)serialises whole to and from the `state` jsonb column of the rollup table — the cursor and the
/// days together — so the persistence seam stores one blob per `(tenant, store)` and rebuilds it
/// intact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredRollups {
    /// The last event folded, or `None` if nothing has been projected yet. Advancing only forward is
    /// what keeps each event counted exactly once.
    pub cursor: Option<EventId>,
    /// The materialised per-trading-day rollups, keyed by `YYYY-MM-DD`.
    pub days: BTreeMap<String, DailyRollup>,
    /// The materialised per-trading-day **revenue** rollups, keyed by `YYYY-MM-DD` (ADR-0081, O4).
    /// `#[serde(default)]` so a blob written before O4 loads with an empty revenue map. Revenue then
    /// accrues from the cursor forward; to backfill a store's historical revenue, reset the rollup
    /// (the ADR-0036 reset-cursor-and-replay lever) so the projector re-folds the whole log.
    #[serde(default)]
    pub revenue: BTreeMap<String, DailyRevenue>,
    /// The materialised per-trading-day **cash-drawer** summaries, keyed by `YYYY-MM-DD` (ADR-0081,
    /// O4). `#[serde(default)]` for the same forward-compatible reason as `revenue`.
    #[serde(default)]
    pub cash: BTreeMap<String, DailyCash>,
}

/// The store that persists materialised rollups (a table in `store-postgres`; a fake in tests).
///
/// Every row is keyed by `(tenant, store)`, and the tenant is never a request parameter: a `/v1`
/// caller's tenant comes from its authenticated [`Grant`](crate::auth::apikey::Grant), so a caller
/// can only ever read the rollups of a store within its own tenant. Guessing another tenant's
/// `store_id` yields the default (empty) rollup, not that tenant's data — the isolation is a fact of
/// the key, not of a check a handler might forget.
pub trait RollupStore {
    /// Loads a store's materialised rollup, or the default (empty, no cursor) if none exists.
    fn load(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<StoredRollups, RollupError>> + Send;

    /// Persists a store's materialised rollup.
    fn save(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        rollups: &StoredRollups,
    ) -> impl Future<Output = Result<(), RollupError>> + Send;
}

/// Why projecting or reading a rollup failed.
#[derive(Debug, thiserror::Error)]
pub enum RollupError {
    /// The event log could not be read.
    #[error("reading the event log failed: {0}")]
    Events(#[from] PortError),
    /// The rollup store could not be read or written.
    #[error("the rollup store failed: {0}")]
    Store(String),
}

impl RollupError {
    /// A rollup-store failure with a human-readable reason.
    #[must_use]
    pub fn store(message: impl Into<String>) -> Self {
        Self::Store(message.into())
    }
}

/// What one [`project`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectReport {
    /// How many new events were folded into the materialised rollup this pass.
    pub folded: u64,
}

/// Brings `store_id`'s materialised rollup up to date by folding every event after its stored cursor.
///
/// The `(tenant, store_id)` pair is the caller's own — the projector is driven per store from the
/// fleet, which knows each store's tenant — and is the key the rollup is stored under, the same key
/// [`dashboard`] reads back for that tenant.
///
/// Idempotent: a second pass with no new events folds nothing and leaves the rollup unchanged,
/// because the cursor only moves forward and the store returns only events after it.
///
/// # Errors
///
/// [`RollupError`] if the event log cannot be read or the rollup store cannot be loaded or saved. A
/// failure leaves the previously-saved rollup intact; the next pass resumes from its cursor.
pub async fn project<E, R>(
    events: &E,
    rollups: &R,
    tenant: TenantId,
    store_id: StoreId,
) -> Result<ProjectReport, RollupError>
where
    E: EventStore,
    R: RollupStore,
{
    let mut state = rollups.load(tenant, store_id).await?;
    let page = NonZeroU32::new(PROJECT_PAGE).unwrap_or(NonZeroU32::MIN);
    let page_len = usize::try_from(PROJECT_PAGE).unwrap_or(usize::MAX);
    let mut folded: u64 = 0;

    loop {
        let mut query = EventQuery::first(store_id, page);
        if let Some(after) = state.cursor {
            query = query.after(after);
        }
        let batch = events.read(&query).await?;
        if batch.is_empty() {
            break;
        }
        for event in &batch {
            fold_event(&mut state.days, event);
            fold_revenue(&mut state.revenue, event);
            fold_cash(&mut state.cash, event);
            folded = folded.saturating_add(1);
        }
        let short = batch.len() < page_len;
        state.cursor = batch.last().map(|event| event.event_id).or(state.cursor);
        if short {
            break;
        }
    }

    rollups.save(tenant, store_id, &state).await?;
    Ok(ProjectReport { folded })
}

/// Answers a store's dashboard from the materialised rollup — oldest trading day first, and with no
/// event-log scan (there is no `EventStore` here to scan).
///
/// `tenant` is the caller's authenticated tenant, so the read is confined to that tenant's rollups:
/// a `store_id` outside the tenant simply has no row and reads back empty.
///
/// # Errors
///
/// [`RollupError::Store`] if the rollup store cannot be read.
pub async fn dashboard<R>(
    rollups: &R,
    tenant: TenantId,
    store_id: StoreId,
    window: &RollupWindow,
) -> Result<Vec<DailyRollup>, RollupError>
where
    R: RollupStore,
{
    let state = rollups.load(tenant, store_id).await?;
    Ok(render_window(state.days, window))
}

/// Answers a store's **revenue** dashboard from the materialised rollup — the recognised-revenue and
/// gross-ordered-mix totals per trading day, windowed exactly as [`dashboard`], with no event-log
/// scan. Revenue is **T2**: the caller must hold `console.reports.revenue` (enforced at the route).
///
/// `tenant` is the caller's tenant, so the read is confined to that tenant's rollups.
///
/// # Errors
///
/// [`RollupError::Store`] if the rollup store cannot be read.
pub async fn revenue<R>(
    rollups: &R,
    tenant: TenantId,
    store_id: StoreId,
    window: &RollupWindow,
) -> Result<Vec<DailyRevenue>, RollupError>
where
    R: RollupStore,
{
    let state = rollups.load(tenant, store_id).await?;
    Ok(render_revenue_window(state.revenue, window))
}

/// Builds an **X or Z report** for one store's trading day (ADR-0081, resolving spec gap D10).
///
/// `business_date` picks the day; absent, it is the latest day present (the current, open day). The
/// report is an **X** when the chosen day is that latest day (still accruing, non-resetting), and a
/// **Z** when it is an earlier — closed — day, whose rollup no longer changes and so reads the same
/// verbatim thereafter. It bundles the day's activity, revenue, and cash summaries; T2 (the caller
/// must hold `console.reports.revenue`, enforced at the route). A day with no data reads back as an
/// empty X.
///
/// # Errors
///
/// [`RollupError::Store`] if the rollup store cannot be read.
pub async fn xz_report<R>(
    rollups: &R,
    tenant: TenantId,
    store_id: StoreId,
    business_date: Option<String>,
) -> Result<XzReport, RollupError>
where
    R: RollupStore,
{
    use super::rollup::{empty_activity, empty_cash, empty_revenue};
    let state = rollups.load(tenant, store_id).await?;
    // The latest trading day with any activity is the current (open) day.
    let latest = state.days.keys().next_back().cloned();
    let day = business_date.or_else(|| latest.clone()).unwrap_or_default();
    // Z for a day strictly before the latest (closed); X for the latest, a future date, or no data.
    let kind = match latest.as_deref() {
        Some(latest_day) if day.as_str() < latest_day => "Z",
        _ => "X",
    };
    Ok(XzReport {
        kind: kind.to_owned(),
        activity: state
            .days
            .get(&day)
            .cloned()
            .unwrap_or_else(|| empty_activity(&day)),
        revenue: state
            .revenue
            .get(&day)
            .cloned()
            .unwrap_or_else(|| empty_revenue(&day)),
        cash: state
            .cash
            .get(&day)
            .cloned()
            .unwrap_or_else(|| empty_cash(&day)),
        business_date: day,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::{RollupError, RollupStore, RollupWindow, StoredRollups, dashboard, project};

    use pos_contract_tests::fixtures;
    use pos_fakes::FakeStore;
    use pos_ports::event_store::EventStore;
    use pos_ports::{Transactional as _, TxContext as _};
    use pos_proto::BusinessDate;
    use pos_proto::envelope::{EventEnvelope, RawPayload};
    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::ulid::Ulid;

    use crate::cloud::Cloud;

    fn store_id() -> StoreId {
        StoreId::new(Ulid::from_u128(0x0ADA))
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(0x7E11A))
    }

    /// A run of activation events re-dated onto one trading day.
    fn dated(
        first_seed: u32,
        count: u32,
        year: i16,
        month: u8,
        day: u8,
    ) -> Vec<EventEnvelope<RawPayload>> {
        let date = BusinessDate::from_ymd(year, month, day).expect("a valid date");
        let mut events = fixtures::activations(store_id(), first_seed, count);
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

    /// An in-memory materialised-rollup store, keyed by `(tenant, store)` exactly as the real table.
    #[derive(Default)]
    struct FakeRollups {
        rows: Mutex<HashMap<(TenantId, StoreId), StoredRollups>>,
    }

    impl RollupStore for FakeRollups {
        async fn load(
            &self,
            tenant: TenantId,
            store_id: StoreId,
        ) -> Result<StoredRollups, RollupError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .get(&(tenant, store_id))
                .cloned()
                .unwrap_or_default())
        }

        async fn save(
            &self,
            tenant: TenantId,
            store_id: StoreId,
            rollups: &StoredRollups,
        ) -> Result<(), RollupError> {
            self.rows
                .lock()
                .expect("lock")
                .insert((tenant, store_id), rollups.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_materialised_rollup_equals_the_from_log_computation() {
        let events = FakeStore::new();
        append(&events, &dated(1, 3, 2026, 3, 15)).await;
        append(&events, &dated(100, 2, 2026, 7, 1)).await;
        let rollups = FakeRollups::default();

        project(&events, &rollups, tenant(), store_id())
            .await
            .expect("project");
        let materialised = dashboard(&rollups, tenant(), store_id(), &RollupWindow::default())
            .await
            .expect("dashboard");

        // The authoritative answer is the full from-log fold. `project` only borrowed `events`, so it
        // can be moved into a Cloud now.
        let authoritative = Cloud::new(events)
            .daily_rollups(store_id())
            .await
            .expect("from-log rollups");
        assert_eq!(
            materialised, authoritative,
            "the maintained rollup must match a full re-scan exactly"
        );
    }

    #[tokio::test]
    async fn a_second_projection_with_no_new_events_folds_nothing() {
        let events = FakeStore::new();
        append(&events, &dated(1, 4, 2026, 3, 15)).await;
        let rollups = FakeRollups::default();

        let first = project(&events, &rollups, tenant(), store_id())
            .await
            .expect("first");
        assert_eq!(first.folded, 4);
        let before = dashboard(&rollups, tenant(), store_id(), &RollupWindow::default())
            .await
            .expect("read");

        let second = project(&events, &rollups, tenant(), store_id())
            .await
            .expect("second");
        assert_eq!(
            second.folded, 0,
            "the cursor is at the end, so nothing is refolded"
        );
        let after = dashboard(&rollups, tenant(), store_id(), &RollupWindow::default())
            .await
            .expect("read");
        assert_eq!(
            before, after,
            "an idempotent projection does not change the rollup"
        );
    }

    #[tokio::test]
    async fn projection_is_incremental_across_appends() {
        let events = FakeStore::new();
        append(&events, &dated(1, 3, 2026, 3, 15)).await;
        let rollups = FakeRollups::default();
        project(&events, &rollups, tenant(), store_id())
            .await
            .expect("first");

        // More events arrive on a second day; only they are folded on the next pass.
        append(&events, &dated(100, 5, 2026, 3, 16)).await;
        let report = project(&events, &rollups, tenant(), store_id())
            .await
            .expect("second");
        assert_eq!(report.folded, 5, "only the new events were folded");

        let days = dashboard(&rollups, tenant(), store_id(), &RollupWindow::default())
            .await
            .expect("read");
        assert_eq!(days.len(), 2);
        let total: u64 = days.iter().map(|day| day.total_events).sum();
        assert_eq!(total, 8, "both days are reflected");
    }

    #[tokio::test]
    async fn the_dashboard_reads_only_the_materialised_store() {
        // Populate the rollup store directly and read it back — there is no event store in this test
        // at all, which is the point: the dashboard cannot be scanning a log it was never given.
        let rollups = FakeRollups::default();
        let mut state = StoredRollups::default();
        crate::dashboard::rollup::fold_event(&mut state.days, &dated(1, 2, 2026, 3, 15)[0]);
        rollups
            .save(tenant(), store_id(), &state)
            .await
            .expect("save");

        let days = dashboard(&rollups, tenant(), store_id(), &RollupWindow::default())
            .await
            .expect("read");
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].business_date, "2026-03-15");
    }

    #[tokio::test]
    async fn xz_report_is_x_for_the_current_day_and_z_for_a_closed_one() {
        let rollups = FakeRollups::default();
        let mut state = StoredRollups::default();
        // Two trading days of activity; the later one is the current (open) day.
        crate::dashboard::rollup::fold_event(&mut state.days, &dated(1, 1, 2026, 3, 14)[0]);
        crate::dashboard::rollup::fold_event(&mut state.days, &dated(2, 1, 2026, 3, 15)[0]);
        rollups
            .save(tenant(), store_id(), &state)
            .await
            .expect("save");

        // No date → the latest day, reported as X (still open).
        let current = super::xz_report(&rollups, tenant(), store_id(), None)
            .await
            .expect("x report");
        assert_eq!(current.business_date, "2026-03-15");
        assert_eq!(current.kind, "X");

        // An earlier day → Z (closed, immutable).
        let closed = super::xz_report(
            &rollups,
            tenant(),
            store_id(),
            Some("2026-03-14".to_owned()),
        )
        .await
        .expect("z report");
        assert_eq!(closed.business_date, "2026-03-14");
        assert_eq!(closed.kind, "Z");
    }
}
