// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud application spine: idempotent ingest, and the rollup read model.
//!
//! [`Cloud`] is generic over the store `S`, so the same ingest and rollup code runs against
//! `pos-fakes` in a test and `store-postgres` in the cloud — static dispatch, no `dyn`
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md), [ADR-0026](../../../docs/adr/0026-port-shapes.md)).

use core::num::NonZeroU32;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use std::sync::Arc;

use pos_ports::dynamic::BoxFuture;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::{PortError, TxContext};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{BrandId, StoreId, TenantId};

/// Who owns a store, as the registry records it
/// ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreOwner {
    /// The tenant that owns the store.
    pub tenant_id: TenantId,
    /// The brand it trades under, or the nil id when it is under none — the registry's `brand_id` is
    /// nullable, and a nil id reads as "no brand" rather than as a brand that might exist.
    pub brand_id: BrandId,
}

/// Resolves a store to its owner, for the stamp
/// [`Cloud::ingest`] puts on every event ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
///
/// `dyn`-compatible on purpose — boxed futures rather than `impl Future` — so [`Cloud`] can hold one
/// without gaining a type parameter that every existing construction site would have to name. The
/// same trade `pos_ports::dynamic` makes, for the same reason.
pub trait StoreOwners: Send + Sync + core::fmt::Debug {
    /// The store's owner, or `None` if the registry has no row for it.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the registry itself cannot be read — distinct from an unknown store, which is
    /// `Ok(None)`.
    fn owner_of(&self, store_id: StoreId) -> BoxFuture<'_, Result<Option<StoreOwner>, PortError>>;
}

/// How many events a rollup pass reads per page from the log.
const ROLLUP_PAGE: u32 = 512;

/// What one ingest achieved.
///
/// Idempotency is the point: re-delivering a batch the cloud already has adds `duplicates`, not
/// `appended`, and grows the log by nothing. At-least-once delivery from the edge
/// ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §4) guarantees the re-delivery happens, so a
/// caller that treats a duplicate as an error turns a healthy retry into a stuck feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestOutcome {
    /// Events newly durable in the cloud.
    pub appended: u32,
    /// Events the cloud already had, discarded.
    pub duplicates: u32,
}

/// A day's activity for one store, derived from the event log.
///
/// The cloud's dashboards answer from rollups rather than by scanning the raw log
/// (`docs/roadmap.md` P7). This first rollup counts events per store trading day and per event
/// type — enough for an activity dashboard, and it uses only envelope fields (`business_date`,
/// `event_type`), so it needs no per-event-type decoding. Money- and order-shaped rollups, and the
/// materialised tables that make the <10 ms dashboard query real, land with the dashboard slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DailyRollup {
    /// The store's trading day, `YYYY-MM-DD` ([ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)).
    pub business_date: String,
    /// Every event recorded on that day.
    pub total_events: u64,
    /// Per-event-type counts, ordered by type so the output is deterministic.
    pub by_type: BTreeMap<String, u64>,
}

/// A day's **revenue** for one store, folded from the settlement and line events
/// ([ADR-0081](../../../docs/adr/0081-reports-and-analytics.md), Track O4).
///
/// Revenue is recognised from `billing.bill.settled` (the settled totals); the product mix is the
/// **gross ordered** mix from `sales.order_line.added` — units and value as ordered, **before** voids
/// and comps are netted (which the line events do not carry back to a menu item), so it reads menu
/// popularity, not per-item recognised revenue. All amounts are the store's single currency's minor
/// units (`docs/pos-spec.md` §19). Prices are **T2**: this rollup, and every route that serves it, is
/// gated behind `console.reports.revenue`. It carries no customer or employee identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DailyRevenue {
    /// The store's trading day, `YYYY-MM-DD`.
    pub business_date: String,
    /// ISO 4217 code of the store's currency, or empty until the first settled bill sets it.
    pub currency_code: String,
    /// Settled bills on the day.
    pub bills: u64,
    /// Sum of settled subtotals (before reductions), minor units.
    pub gross: i64,
    /// Sum of reductions (discounts + comps), minor units.
    pub reductions: i64,
    /// Sum of service charge, minor units.
    pub service_charge: i64,
    /// Sum of tax, minor units.
    pub tax: i64,
    /// Sum of `total_due` — what guests owed — minor units. The headline "revenue" figure.
    pub net: i64,
    /// Gross ordered mix, keyed by `menu_item_id`, ordered by key for determinism.
    pub by_item: BTreeMap<String, ItemMix>,
}

/// One menu item's gross ordered contribution on a trading day (part of [`DailyRevenue`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
pub struct ItemMix {
    /// The most recent display name seen for the item, for a human-readable report.
    pub name: String,
    /// Sum of ordered quantity, in thousandths of a unit (`Quantity::milli`).
    pub ordered_qty_milli: i64,
    /// Sum of ordered line totals, minor units — gross, before voids/comps.
    pub ordered_value: i64,
}

/// A day's **cash-drawer** summary for one store, folded from the shift and drawer events
/// ([ADR-0081](../../../docs/adr/0081-reports-and-analytics.md), Track O4). Amounts are the store's
/// single currency's minor units. Part of an X/Z report; T2 (it exposes money).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
pub struct DailyCash {
    /// The store's trading day, `YYYY-MM-DD`.
    pub business_date: String,
    /// ISO 4217 code, or empty until a cash event on the day sets it.
    pub currency_code: String,
    /// Sum of opening floats across shifts opened on the day.
    pub opening_float: i64,
    /// Sum of paid-in movements.
    pub paid_in: i64,
    /// Sum of paid-out movements.
    pub paid_out: i64,
    /// Shifts opened on the day.
    pub shifts_opened: u64,
    /// Shifts closed on the day.
    pub shifts_closed: u64,
    /// Sum of expected drawer amounts across closes.
    pub expected: i64,
    /// Sum of counted amounts across closes (the blind counts).
    pub counted: i64,
    /// Sum of variance (counted − expected) across closes; negative is short.
    pub variance: i64,
}

/// An **X or Z report** for one store's trading day (ADR-0081, Track O4, resolving spec gap D10).
///
/// An **X** report is the current (open) day's running totals — non-resetting, recomputed each call.
/// A **Z** report is a closed day's totals — a day that no longer receives events, so the same read
/// returns the same figures verbatim thereafter. `kind` is `"X"` for the latest day present and
/// `"Z"` for any earlier (closed) day. It bundles the day's activity counts, revenue, and cash
/// summary; T2, so it is served only behind `console.reports.revenue`. Not a legal fiscal document —
/// that stays with the country module's invoice range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct XzReport {
    /// `"X"` (current, interim) or `"Z"` (closed, final).
    pub kind: String,
    /// The trading day, `YYYY-MM-DD`.
    pub business_date: String,
    /// The day's event-activity counts.
    pub activity: DailyRollup,
    /// The day's recognised revenue and product mix.
    pub revenue: DailyRevenue,
    /// The day's cash-drawer summary.
    pub cash: DailyCash,
}

/// The cloud's application layer over an [`EventStore`].
///
/// Cloneable and shareable — every clone talks to the same store.
#[derive(Debug, Clone)]
pub struct Cloud<S> {
    store: S,
    /// The registry [`Self::ingest`] stamps each event's owner from
    /// ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    ///
    /// `None` leaves each envelope's claimed tenant and brand alone, which is what a fakes-backed
    /// test and the on-fakes example want — they have no registry to resolve against. **The shipped
    /// binary always sets one**: without it the event log's tenant column is whatever the box asserted,
    /// which is the hole S2 names.
    owners: Option<Arc<dyn StoreOwners>>,
}

impl<S: EventStore> Cloud<S> {
    /// Builds the application over `store`, taking each event's tenant and brand as the envelope
    /// claims them.
    ///
    /// For a test and for the on-fakes example. The shipped cloud uses
    /// [`with_store_owners`](Self::with_store_owners) (ADR-0101).
    #[must_use]
    pub fn new(store: S) -> Self {
        Self {
            store,
            owners: None,
        }
    }

    /// Builds the application over `store`, stamping each ingested event's tenant and brand from the
    /// store registry ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    ///
    /// This is the shipped composition. The tenant on an event is the column row-level isolation is
    /// defined on, and until this existed it was whatever the publishing box put there — one constant
    /// for the whole fleet, as it happened.
    #[must_use]
    pub fn with_store_owners(store: S, owners: Arc<dyn StoreOwners>) -> Self {
        Self {
            store,
            owners: Some(owners),
        }
    }

    /// The underlying store.
    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Ingests a batch, idempotently, in one transaction.
    ///
    /// The store's `append` is idempotent by `event_id` ([ADR-0026](../../../docs/adr/0026-port-shapes.md)),
    /// so a replay stores nothing and is reported as `duplicates`. The whole batch commits or none
    /// of it does.
    ///
    /// # The tenant is the registry's, not the envelope's
    ///
    /// Every event is stamped with the tenant and brand the store registry records for its `store_id`,
    /// overwriting what the envelope claimed ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    /// This is the single funnel both ingest paths pass through — the NATS cursor and
    /// `POST /internal/ingest` — so it is the one place the stamp has to be. Idempotency is unaffected:
    /// the log's key is `(business_date, event_id)` and carries no tenant.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached or the transaction fails; nothing is committed.
    /// A registry that cannot be read is **not** an error — the batch is stored with the tenants it
    /// claimed and the failure is logged, because refusing would stall the fleet's ingest behind a
    /// lookup, and dropping would lose a real store's trading history.
    pub async fn ingest(
        &self,
        events: &[EventEnvelope<RawPayload>],
    ) -> Result<IngestOutcome, PortError> {
        if events.is_empty() {
            return Ok(IngestOutcome::default());
        }
        let stamped = self.stamp_owners(events).await;
        let mut tx = self.store.begin().await?;
        let outcome = self.store.append(&mut tx, &stamped).await?;
        tx.commit().await?;
        Ok(IngestOutcome {
            appended: outcome.appended,
            duplicates: outcome.duplicates,
        })
    }

    /// Rewrites each event's tenant and brand to the registry's answer for its store (ADR-0101).
    ///
    /// One lookup per distinct store in the batch, not per event: a batch is one store's window far
    /// more often than not, and a fleet-wide batch is still bounded by the number of stores in it.
    ///
    /// Three things are deliberately not errors, and each keeps its own claim:
    /// - **No registry composed** — a test or the on-fakes example.
    /// - **The registry cannot be read** — refusing would stall every store's ingest behind one
    ///   unavailable lookup, and the log is the thing that must not stop.
    /// - **The store is unknown** — a provisioned store has a registry row by construction, so this
    ///   is a diagnostic rather than a path; losing a real store's history to a missing row would be
    ///   a provisioning bug turned into data loss.
    async fn stamp_owners(
        &self,
        events: &[EventEnvelope<RawPayload>],
    ) -> Vec<EventEnvelope<RawPayload>> {
        let mut stamped = events.to_vec();
        let Some(owners) = self.owners.as_ref() else {
            return stamped;
        };
        let mut resolved: BTreeMap<StoreId, Option<StoreOwner>> = BTreeMap::new();
        for event in &mut stamped {
            let owner = if let Some(owner) = resolved.get(&event.store_id) {
                *owner
            } else {
                let owner = match owners.owner_of(event.store_id).await {
                    Ok(owner) => owner,
                    Err(error) => {
                        tracing::warn!(
                            store = %event.store_id,
                            %error,
                            "could not resolve a store's owner; ingesting with the tenant the event claimed"
                        );
                        None
                    }
                };
                if owner.is_none() {
                    tracing::warn!(
                        store = %event.store_id,
                        "no registry row for the store that published this event; ingesting with the tenant it claimed"
                    );
                }
                resolved.insert(event.store_id, owner);
                owner
            };
            if let Some(owner) = owner {
                event.tenant_id = owner.tenant_id;
                event.brand_id = owner.brand_id;
            }
        }
        stamped
    }

    /// The per-day activity rollup for one store, oldest day first.
    ///
    /// Reads the store's log in pages and folds each event into its trading day with the shared
    /// [`fold_event`](crate::dashboard::rollup::fold_event). This computes the rollup from the log
    /// every call — correct but O(events); the dashboard read path materialises the *same* fold and
    /// answers in O(days) ([`crate::dashboard`], [ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached.
    pub async fn daily_rollups(&self, store_id: StoreId) -> Result<Vec<DailyRollup>, PortError> {
        let page = NonZeroU32::new(ROLLUP_PAGE).unwrap_or(NonZeroU32::MIN);
        let page_len = usize::try_from(ROLLUP_PAGE).unwrap_or(usize::MAX);
        let mut days: BTreeMap<String, DailyRollup> = BTreeMap::new();
        let mut cursor = None;
        loop {
            let mut query = EventQuery::first(store_id, page);
            if let Some(after) = cursor {
                query = query.after(after);
            }
            let batch = self.store.read(&query).await?;
            if batch.is_empty() {
                break;
            }
            for event in &batch {
                crate::dashboard::rollup::fold_event(&mut days, event);
            }
            let short = batch.len() < page_len;
            cursor = batch.last().map(|event| event.event_id);
            if short {
                break;
            }
        }
        Ok(crate::dashboard::rollup::render(days))
    }
}
