// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cash dropped from a drawer to the safe (`cash.drawer.dropped`,
//! [ADR-0186](../../../../docs/adr/0186-a-drawer-is-counted-by-denomination.md) decision 5).
//!
//! It leaves the drawer like a paid out, and the close's expected amount already leaves it out, but
//! it is no expense: the day and each drawer carry it as `dropped`, apart from `paid_out`, so the
//! console shows "Dropped to safe" beside the expenses rather than inside them. One event at a time,
//! as every fold here ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)).

use std::collections::BTreeMap;

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::CashDrawerDropped;

use super::rollup::{empty_cash, set_currency};
use crate::types::{DailyCash, STORE_DRAWER};

/// Folds a drop into its day's and its drawer's `dropped`. Every other event is ignored, and a
/// payload that fails to decode is skipped, as the other folds skip one.
pub fn fold_drops(cash: &mut BTreeMap<String, DailyCash>, event: &EventEnvelope<RawPayload>) {
    if event.event_type.as_str() != "cash.drawer.dropped" {
        return;
    }
    let Ok(dropped) = event.data.decode::<CashDrawerDropped>() else {
        return;
    };
    let date = event.business_date.to_string();
    let day = cash
        .entry(date.clone())
        .or_insert_with(|| empty_cash(&date));
    set_currency(
        &mut day.currency_code,
        dropped.amount.currency_code.to_string(),
    );
    let amount = dropped.amount.amount_minor;
    day.dropped = day.dropped.saturating_add(amount);
    let drawer = dropped
        .terminal_device_id
        .map_or_else(|| STORE_DRAWER.to_owned(), |till| till.to_string());
    let figures = day.by_drawer.entry(drawer).or_default();
    figures.dropped = figures.dropped.saturating_add(amount);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pos_contract_tests::fixtures;
    use pos_proto::envelope::{EventTypeRef, RawPayload};
    use pos_proto::events::EventType;
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;
    use serde_json::json;

    use super::fold_drops;
    use crate::dashboard::rollup::fold_cash;
    use crate::types::STORE_DRAWER;

    #[test]
    fn a_drop_is_carried_apart_from_the_paid_out_for_the_day_and_its_drawer() {
        let till = Ulid::from_u128(0xBA).to_string();
        let mut drop = fixtures::activations(StoreId::new(Ulid::from_u128(1)), 1, 1).remove(0);
        drop.event_type = EventTypeRef::from_known(EventType::CashDrawerDropped);
        drop.data = RawPayload::encode(&json!({
            "amount": { "currency_code": "VND", "amount_minor": 1_000_000 },
            "terminal_device_id": till,
        }))
        .expect("encode");
        let mut cash = BTreeMap::new();
        fold_cash(&mut cash, &drop);
        fold_drops(&mut cash, &drop);
        let day = cash.values().next().expect("the drop makes its day");
        assert_eq!((day.dropped, day.paid_out), (1_000_000, 0));
        assert_eq!(day.currency_code, "VND");
        assert_eq!(day.by_drawer[&till].dropped, 1_000_000);
        assert!(!day.by_drawer.contains_key(STORE_DRAWER));
        let wire = serde_json::to_value(day).expect("serialise");
        assert_eq!(wire["dropped"], 1_000_000);
    }
}
