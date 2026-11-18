// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Overriding a line's price before its bill opens
//! ([ADR-0219](../../../../docs/adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md)
//! decision 2).
//!
//! Two things say no, in this order: the bounds, then the permission with its PIN. The bounds come
//! first because no approval changes them, so a manager is not asked to approve a price that would
//! be refused anyway. Whether the bill is open, and which reason is cited, are the caller's to read
//! and refuse; the core decides from values.

use pos_proto::money::Money;

use super::{DecisionCtx, require_with_pin};
use crate::error::DomainError;
use crate::permission::Permission;

/// What a price override is decided on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reprice {
    /// The new price of one.
    pub unit_price: Money,
    /// The price of one the line was added at, which the ceiling is a multiple of. Never a price
    /// an earlier override set, so two overrides cannot climb past the ceiling between them.
    pub added_unit_price: Money,
    /// The store's `billing.price_override_max_multiple`.
    pub max_multiple: i64,
    /// Whether the edge has collected and verified a PIN for this act.
    pub pin_verified: bool,
}

/// Whether `unit_price` is a price a line added at `added_unit_price` may be overridden to: above
/// zero, in the same currency, and at most `max_multiple` times the price it was added at. A zero
/// price is refused because giving an item away is a comp, with its own event and permission.
///
/// # Errors
///
/// [`DomainError::PriceOutOfRange`] naming the ceiling; [`DomainError::Money`] for another currency
/// or a ceiling that overflows.
pub fn reprice_bounds(
    unit_price: Money,
    added_unit_price: Money,
    max_multiple: i64,
) -> Result<(), DomainError> {
    let ceiling = added_unit_price.checked_mul(max_multiple)?;
    // Same currency, or the money primitive refuses: a ceiling in đồng says nothing about cents.
    let headroom = ceiling.checked_sub(unit_price)?;
    if unit_price.amount_minor <= 0 || headroom.is_negative() {
        return Err(DomainError::PriceOutOfRange {
            ceiling_minor: ceiling.amount_minor,
        });
    }
    Ok(())
}

/// Decides a price override: the bounds, then [`Permission::OverridePrice`] with its PIN.
///
/// # Errors
///
/// What [`reprice_bounds`] refuses, then [`DomainError::PermissionDenied`] when the permission is not
/// granted or its PIN was not verified.
pub fn decide_reprice(reprice: Reprice, ctx: &DecisionCtx) -> Result<(), DomainError> {
    reprice_bounds(
        reprice.unit_price,
        reprice.added_unit_price,
        reprice.max_multiple,
    )?;
    require_with_pin(ctx, Permission::OverridePrice, reprice.pin_verified)
}

#[cfg(test)]
#[path = "tests/reprice.rs"]
mod tests;
