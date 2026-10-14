// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The daily rollups computed from the log every call, which the materialised ones must equal.

use core::num::NonZeroU32;
use std::collections::BTreeMap;

use pos_ports::PortError;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::StoreId;

use super::rollup::{fold_event, render};
use crate::types::DailyRollup;

/// How many events a rollup pass reads per page from the log.
const ROLLUP_PAGE: u32 = 512;

/// The per-day activity rollup for one store, oldest day first, from `events`' log.
///
/// Reads the store's log in pages and folds each event into its trading day with [`fold_event`].
/// This computes the rollup from the log every call — correct but O(events); the dashboard read path
/// materialises the *same* fold and answers in O(days) ([`crate::dashboard`],
/// [ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)).
///
/// # Errors
///
/// [`PortError`] if the store cannot be reached.
pub async fn daily_rollups_from_log<E: EventStore>(
    events: &E,
    store_id: StoreId,
) -> Result<Vec<DailyRollup>, PortError> {
    let page = NonZeroU32::new(ROLLUP_PAGE).unwrap_or(NonZeroU32::MIN);
    let page_len = usize::try_from(ROLLUP_PAGE).unwrap_or(usize::MAX);
    let mut days: BTreeMap<String, DailyRollup> = BTreeMap::new();
    let mut cursor = None;
    loop {
        let mut query = EventQuery::first(store_id, page);
        if let Some(after) = cursor {
            query = query.after(after);
        }
        let batch = events.read(&query).await?;
        if batch.is_empty() {
            break;
        }
        for event in &batch {
            fold_event(&mut days, event);
        }
        let short = batch.len() < page_len;
        cursor = batch.last().map(|event| event.event_id);
        if short {
            break;
        }
    }
    Ok(render(days))
}
