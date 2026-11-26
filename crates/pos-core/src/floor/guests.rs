// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Recording the guest count of a table already seated
//! ([ADR-0254](../../../../docs/adr/0254-a-seated-tables-guest-count-is-recorded-in-place.md)).
//!
//! A flow can ask for the count before the first dish; a table seated without it is given one in
//! place, rather than released and seated again. The table does not move: the count is recorded on
//! its open order while its guests are there, seated or waiting to pay.

use pos_proto::{TableState, WireEnum};

use crate::capability::Capability;
use crate::decision::DecisionCtx;
use crate::error::DomainError;
use crate::permission::Permission;
use crate::state_machine::TransitionError;

/// Decides whether `guest_count` may be recorded on a table in state `current`. Needs table
/// service and [`Permission::ManageTables`], the seat's own.
///
/// # Errors
///
/// - [`DomainError::CapabilityDisabled`] if `tables_enabled` is off.
/// - [`DomainError::Empty`] for a count of zero: a count names at least one guest.
/// - [`DomainError::Transition`] unless the table is occupied or awaiting payment.
/// - [`DomainError::PermissionDenied`] without `sales.table.manage`.
pub fn decide_guest_count(
    current: TableState,
    guest_count: u16,
    ctx: &DecisionCtx,
) -> Result<(), DomainError> {
    ctx.require_capability(Capability::Tables)?;
    if guest_count == 0 {
        return Err(DomainError::Empty {
            what: "guest count",
        });
    }
    if !matches!(current, TableState::Occupied | TableState::AwaitingPayment) {
        return Err(DomainError::Transition(TransitionError {
            machine: "table",
            from: current.as_wire(),
            trigger: "count_guests",
        }));
    }
    ctx.require(Permission::ManageTables)?;
    Ok(())
}

#[cfg(test)]
mod tests;
