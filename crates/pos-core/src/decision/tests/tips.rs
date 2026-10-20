// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The decision tests for a tip: settling with one needs the store to be taking tips, and
//! settling without one does not.

use super::*;
use crate::decision::BillDecision;

/// One cash tender covering `applied` and carrying `tip` on top.
fn tipped(applied: i64, tip: i64) -> Payment {
    Payment {
        method: PaymentMethod::Cash,
        tendered: vnd(applied + tip),
        applied_to_bill: vnd(applied),
        tip: vnd(tip),
    }
}

fn settle_with(
    payment: Payment,
    capabilities: CapabilityContext,
) -> Result<BillDecision, DomainError> {
    let ctx = ctx_with(
        PermissionSet::EMPTY.with(Permission::TakePayment),
        capabilities,
    );
    decide_bill(
        BillState::Open,
        BillCommand::Settle {
            total_due: vnd(100_000),
            payments: vec![payment],
        },
        &ctx,
    )
}

#[test]
fn a_tip_is_refused_where_the_store_takes_none_and_the_untipped_bill_settles() {
    assert!(matches!(
        settle_with(tipped(100_000, 10_000), CapabilityContext::NONE),
        Err(DomainError::CapabilityDisabled { .. })
    ));
    assert!(settle_with(tipped(100_000, 0), CapabilityContext::NONE).is_ok());
    let tips = CapabilityContext::NONE.with(Capability::Tips);
    let decision = settle_with(tipped(100_000, 10_000), tips).expect("settles with the tip");
    assert_eq!(decision.next_state, BillState::Settled);
}
