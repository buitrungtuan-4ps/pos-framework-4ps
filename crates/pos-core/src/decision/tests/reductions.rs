// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The decision tests for reductions: a discount and a comp against the bill's state, the
//! permission each needs, the ceiling, and what the bill can still be reduced by.

use super::*;

/// A reduction on a 100,000 bill, with a manager standing there and a 50,000 ceiling, so each
/// test below varies exactly the one thing it is about.
fn reduce(kind: ReductionKind, amount: i64, applied: i64) -> BillCommand {
    BillCommand::Reduce {
        kind,
        amount: vnd(amount),
        applied: vnd(applied),
        reducible: vnd(100_000),
        ceiling: Some(vnd(50_000)),
        pin_verified: true,
    }
}

/// A discount under the ceiling is ordinary work: `billing.discount.apply` is granted to a
/// server and is **not** PIN-flagged, so no manager is fetched for taking 20,000 off.
#[test]
fn a_discount_within_the_ceiling_needs_no_manager() {
    let ctx = ctx_with(
        PermissionSet::EMPTY.with(Permission::ApplyDiscount),
        CapabilityContext::NONE,
    );
    let alone = BillCommand::Reduce {
        kind: ReductionKind::Discount,
        amount: vnd(20_000),
        applied: vnd(0),
        reducible: vnd(100_000),
        ceiling: Some(vnd(50_000)),
        pin_verified: false,
    };
    assert!(decide_bill(BillState::Open, alone, &ctx).is_ok());
}

/// Above it, it is a different act. `billing.discount.override_ceiling` is manager-only and
/// PIN-flagged, and holding the ordinary discount permission does not stand in for it.
#[test]
fn a_discount_above_the_ceiling_needs_the_override_and_its_pin() {
    let server = ctx_with(
        PermissionSet::EMPTY.with(Permission::ApplyDiscount),
        CapabilityContext::NONE,
    );
    assert!(matches!(
        decide_bill(
            BillState::Open,
            reduce(ReductionKind::Discount, 60_000, 0),
            &server
        ),
        Err(DomainError::PermissionDenied {
            permission: "billing.discount.override_ceiling"
        })
    ));

    let manager = ctx_with(
        PermissionSet::EMPTY
            .with(Permission::ApplyDiscount)
            .with(Permission::OverrideDiscountCeiling),
        CapabilityContext::NONE,
    );
    assert!(
        decide_bill(
            BillState::Open,
            reduce(ReductionKind::Discount, 60_000, 0),
            &manager
        )
        .is_ok()
    );

    // Held but not verified is not held: the override is PIN-flagged.
    let unverified = BillCommand::Reduce {
        kind: ReductionKind::Discount,
        amount: vnd(60_000),
        applied: vnd(0),
        reducible: vnd(100_000),
        ceiling: Some(vnd(50_000)),
        pin_verified: false,
    };
    assert!(matches!(
        decide_bill(BillState::Open, unverified, &manager),
        Err(DomainError::PermissionDenied {
            permission: "billing.discount.override_ceiling"
        })
    ));
}

/// The direction that matters most. No store publishes a discount ceiling today — nothing in
/// the cloud authors one — and `billing.discount.apply` is granted to a server with no PIN. If
/// an absent ceiling meant "no limit", every server would hold an unbounded till on the day
/// this shipped. It means zero.
#[test]
fn a_store_that_has_published_no_ceiling_has_a_ceiling_of_zero() {
    let server = ctx_with(
        PermissionSet::EMPTY.with(Permission::ApplyDiscount),
        CapabilityContext::NONE,
    );
    let any_discount = BillCommand::Reduce {
        kind: ReductionKind::Discount,
        amount: vnd(1),
        applied: vnd(0),
        reducible: vnd(100_000),
        ceiling: None,
        pin_verified: true,
    };
    assert!(matches!(
        decide_bill(BillState::Open, any_discount, &server),
        Err(DomainError::PermissionDenied {
            permission: "billing.discount.override_ceiling"
        })
    ));

    // And a discount of nothing is not a discount, so it is not an override either.
    let nothing = BillCommand::Reduce {
        kind: ReductionKind::Discount,
        amount: vnd(0),
        applied: vnd(0),
        reducible: vnd(100_000),
        ceiling: None,
        pin_verified: false,
    };
    assert!(decide_bill(BillState::Open, nothing, &server).is_ok());
}

/// The two reductions are two permissions, and holding one does not grant the other. A server
/// who may knock money off a price may not give the food away.
#[test]
fn a_comp_is_not_a_discount_and_neither_grants_the_other() {
    let discounter = ctx_with(
        PermissionSet::EMPTY.with(Permission::ApplyDiscount),
        CapabilityContext::NONE,
    );
    assert!(matches!(
        decide_bill(
            BillState::Open,
            reduce(ReductionKind::Comp, 20_000, 0),
            &discounter
        ),
        Err(DomainError::PermissionDenied {
            permission: "billing.comp.apply"
        })
    ));

    let comper = ctx_with(
        PermissionSet::EMPTY.with(Permission::ApplyComp),
        CapabilityContext::NONE,
    );
    assert!(matches!(
        decide_bill(
            BillState::Open,
            // Under the ceiling, so the refusal is about the discount permission itself
            // rather than about the override.
            reduce(ReductionKind::Discount, 20_000, 0),
            &comper
        ),
        Err(DomainError::PermissionDenied {
            permission: "billing.discount.apply"
        })
    ));
}

/// The sum is what is checked, not the reduction in hand. This is the case a per-reduction
/// check would pass: two discounts, each well under the bill, that together exceed it.
#[test]
fn two_legal_reductions_can_be_illegal_together() {
    // A manager, so the ceiling is not what this test is measuring.
    let ctx = ctx_with(
        PermissionSet::EMPTY
            .with(Permission::ApplyDiscount)
            .with(Permission::OverrideDiscountCeiling),
        CapabilityContext::NONE,
    );
    // 60,000 off a 100,000 bill: fine.
    assert!(
        decide_bill(
            BillState::Open,
            reduce(ReductionKind::Discount, 60_000, 0),
            &ctx
        )
        .is_ok()
    );
    // Another 60,000 on top of it: not fine, although 60,000 alone was.
    assert!(matches!(
        decide_bill(
            BillState::Open,
            reduce(ReductionKind::Discount, 60_000, 60_000),
            &ctx
        ),
        Err(DomainError::ReductionExceedsBill {
            reduction_minor: 120_000,
            reducible_minor: 100_000,
        })
    ));
    // And exactly the bill is allowed: a guest can be given the whole thing.
    assert!(
        decide_bill(
            BillState::Open,
            reduce(ReductionKind::Discount, 40_000, 60_000),
            &ctx
        )
        .is_ok()
    );
}

/// A settled bill is not discounted, it is refunded (ADR-0028), and a voided one is owed
/// nothing to reduce. Both refusals are the machine's, not a permission's — there is no PIN
/// that makes either legal.
#[test]
fn a_bill_that_is_no_longer_open_cannot_be_reduced() {
    let ctx = ctx_with(
        PermissionSet::EMPTY
            .with(Permission::ApplyDiscount)
            .with(Permission::ApplyComp),
        CapabilityContext::NONE,
    );
    for state in [BillState::Settled, BillState::Voided] {
        assert!(matches!(
            decide_bill(state, reduce(ReductionKind::Discount, 1_000, 0), &ctx),
            Err(DomainError::Transition(_))
        ));
    }
}

/// `ReductionKind` carries a third value the wire needs and this command does not: a void is
/// `BillCommand::Void`, with its own permission and its own terminal state. An unknown token
/// from a newer sender lands in the same arm, which is the safe direction.
#[test]
fn a_void_is_not_a_reduction_command() {
    let ctx = ctx_with(
        PermissionSet::EMPTY
            .with(Permission::ApplyDiscount)
            .with(Permission::ApplyComp),
        CapabilityContext::NONE,
    );
    assert!(matches!(
        decide_bill(BillState::Open, reduce(ReductionKind::Void, 1_000, 0), &ctx),
        Err(DomainError::PermissionDenied { .. })
    ));
}
