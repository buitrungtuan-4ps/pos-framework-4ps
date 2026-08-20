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

use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::{PortError, TxContext};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::StoreId;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailyRollup {
    /// The store's trading day, `YYYY-MM-DD` ([ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)).
    pub business_date: String,
    /// Every event recorded on that day.
    pub total_events: u64,
    /// Per-event-type counts, ordered by type so the output is deterministic.
    pub by_type: BTreeMap<String, u64>,
}

/// The cloud's application layer over an [`EventStore`].
///
/// Cloneable and shareable — every clone talks to the same store.
#[derive(Debug, Clone)]
pub struct Cloud<S> {
    store: S,
}

impl<S: EventStore> Cloud<S> {
    /// Builds the application over `store`.
    #[must_use]
    pub fn new(store: S) -> Self {
        Self { store }
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
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached or the transaction fails; nothing is committed.
    pub async fn ingest(
        &self,
        events: &[EventEnvelope<RawPayload>],
    ) -> Result<IngestOutcome, PortError> {
        if events.is_empty() {
            return Ok(IngestOutcome::default());
        }
        let mut tx = self.store.begin().await?;
        let outcome = self.store.append(&mut tx, events).await?;
        tx.commit().await?;
        Ok(IngestOutcome {
            appended: outcome.appended,
            duplicates: outcome.duplicates,
        })
    }

    /// The per-day activity rollup for one store, oldest day first.
    ///
    /// Reads the store's log in pages and folds each event into its trading day. Correctness, not
    /// speed: this is the read model the dashboard slice will materialise, but computing it from the
    /// log keeps it verifiable against the in-memory fake here.
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
                let day = days
                    .entry(event.business_date.to_string())
                    .or_insert_with(|| DailyRollup {
                        business_date: event.business_date.to_string(),
                        total_events: 0,
                        by_type: BTreeMap::new(),
                    });
                day.total_events = day.total_events.saturating_add(1);
                let type_count = day
                    .by_type
                    .entry(event.event_type.as_str().to_owned())
                    .or_insert(0);
                *type_count = type_count.saturating_add(1);
            }
            let short = batch.len() < page_len;
            cursor = batch.last().map(|event| event.event_id);
            if short {
                break;
            }
        }
        Ok(days.into_values().collect())
    }
}
