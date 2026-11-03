// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What each tender captured on a day, for the X/Z report's tender split
//! ([ADR-0187](../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)).
//!
//! One event at a time ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)): a captured
//! payment adds what it applied to its bill under its method's token. A declined or unknown
//! outcome moved no money, or none anybody knows of, and adds nothing.

use std::collections::BTreeMap;

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::BillingPaymentCaptured;
use pos_proto::{PaymentOutcome, WireEnum};

use super::rollup::{empty_revenue, set_currency};
use crate::types::DailyRevenue;

/// Folds a captured payment into its day's `by_tender`. Every other event is ignored, and a payload
/// that fails to decode is skipped, as the other folds skip one.
pub fn fold_tenders(
    revenue: &mut BTreeMap<String, DailyRevenue>,
    event: &EventEnvelope<RawPayload>,
) {
    if event.event_type.as_str() != "billing.payment.captured" {
        return;
    }
    let Ok(payment) = event.data.decode::<BillingPaymentCaptured>() else {
        return;
    };
    if payment.outcome.known() != PaymentOutcome::Captured {
        return;
    }
    let date = event.business_date.to_string();
    let day = revenue
        .entry(date.clone())
        .or_insert_with(|| empty_revenue(&date));
    let applied = payment.applied_to_bill;
    set_currency(&mut day.currency_code, applied.currency_code.to_string());
    let slot = day
        .by_tender
        .entry(payment.method.known().as_wire().to_owned())
        .or_insert(0);
    *slot = slot.saturating_add(applied.amount_minor);
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

    use super::fold_tenders;

    fn captured(
        method: &str,
        outcome: &str,
        applied: i64,
    ) -> pos_proto::envelope::EventEnvelope<RawPayload> {
        let money = |minor: i64| json!({ "currency_code": "VND", "amount_minor": minor });
        let mut base = fixtures::activations(StoreId::new(Ulid::from_u128(1)), 1, 1).remove(0);
        base.event_type = EventTypeRef::from_known(EventType::BillingPaymentCaptured);
        base.data = RawPayload::encode(&json!({
            "bill_id": Ulid::from_u128(5).to_string(),
            "payment_id": Ulid::from_u128(6).to_string(),
            "method": method,
            "outcome": outcome,
            "tendered": money(applied + 35_000),
            "applied_to_bill": money(applied),
            "change_given": money(35_000),
            "tip_amount": money(0),
        }))
        .expect("encode");
        base
    }

    #[test]
    fn each_tender_adds_what_it_applied_and_a_payment_that_moved_nothing_adds_nothing() {
        let mut revenue = BTreeMap::new();
        for event in [
            captured("PAYMENT_METHOD_CASH", "PAYMENT_OUTCOME_CAPTURED", 165_000),
            captured("PAYMENT_METHOD_CASH", "PAYMENT_OUTCOME_CAPTURED", 100_000),
            captured("PAYMENT_METHOD_CARD", "PAYMENT_OUTCOME_CAPTURED", 250_000),
            captured("PAYMENT_METHOD_QR", "PAYMENT_OUTCOME_DECLINED", 99_000),
        ] {
            fold_tenders(&mut revenue, &event);
        }
        let day = revenue.values().next().expect("the day");
        assert_eq!(
            day.by_tender["PAYMENT_METHOD_CASH"], 265_000,
            "change apart"
        );
        assert_eq!(day.by_tender["PAYMENT_METHOD_CARD"], 250_000);
        assert!(!day.by_tender.contains_key("PAYMENT_METHOD_QR"), "declined");
    }
}
