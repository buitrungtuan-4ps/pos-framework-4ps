// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The decision tests for a price override (ADR-0219 decision 2): the bounds first, then the
//! permission with its PIN.

use super::*;
use crate::decision::{Reprice, decide_reprice};

/// A line added at 50,000, at a store that allows three times the price.
fn reprice(unit_price: i64, pin_verified: bool) -> Reprice {
    Reprice {
        unit_price: vnd(unit_price),
        added_unit_price: vnd(50_000),
        max_multiple: 3,
        pin_verified,
    }
}

fn manager() -> DecisionCtx {
    ctx_with(
        PermissionSet::EMPTY.with(Permission::OverridePrice),
        CapabilityContext::NONE,
    )
}

#[test]
fn a_price_up_to_the_multiple_is_overridden_with_a_pin() {
    assert!(decide_reprice(reprice(150_000, true), &manager()).is_ok());
    assert!(decide_reprice(reprice(1, true), &manager()).is_ok());
}

#[test]
fn a_price_of_nothing_or_over_the_multiple_is_refused_before_anybody_approves() {
    for refused in [0, -1, 150_001] {
        assert_eq!(
            decide_reprice(reprice(refused, false), &manager()),
            Err(DomainError::PriceOutOfRange {
                ceiling_minor: 150_000
            }),
            "{refused}"
        );
    }
}

#[test]
fn an_override_needs_the_permission_and_its_pin() {
    assert!(matches!(
        decide_reprice(reprice(60_000, false), &manager()),
        Err(DomainError::PermissionDenied {
            permission: "billing.price.override"
        })
    ));
    let server = ctx_with(
        PermissionSet::EMPTY.with(Permission::AddLine),
        CapabilityContext::NONE,
    );
    assert!(decide_reprice(reprice(60_000, true), &server).is_err());
}
