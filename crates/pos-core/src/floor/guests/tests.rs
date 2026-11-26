// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A seated table's guest count, recorded in place (ADR-0254).

use pos_proto::ids::{DeviceId, EmployeeId};
use pos_proto::money::CurrencyCode;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::{TableState, Ulid};

use super::decide_guest_count;
use crate::campaign::Connectivity;
use crate::capability::{Capability, CapabilityContext};
use crate::decision::{Actor, DecisionCtx};
use crate::error::DomainError;
use crate::permission::{Permission, PermissionSet};

fn ctx(granted: PermissionSet, capabilities: CapabilityContext) -> DecisionCtx {
    DecisionCtx {
        now: Timestamp::EPOCH,
        business_date: BusinessDate::from_ymd(2026, 11, 26).expect("valid date"),
        actor: Actor {
            employee_id: EmployeeId::new(Ulid::from_u128(1)),
            device_id: DeviceId::new(Ulid::from_u128(2)),
        },
        granted,
        granted_directly: PermissionSet::EMPTY,
        capabilities,
        connectivity: Connectivity::Online,
        currency: CurrencyCode::VND,
    }
}

fn waiter() -> DecisionCtx {
    ctx(
        PermissionSet::EMPTY.with(Permission::ManageTables),
        CapabilityContext::NONE.with(Capability::Tables),
    )
}

#[test]
fn a_seated_or_billed_table_takes_a_count() {
    let ctx = waiter();
    assert_eq!(decide_guest_count(TableState::Occupied, 4, &ctx), Ok(()));
    assert_eq!(
        decide_guest_count(TableState::AwaitingPayment, 2, &ctx),
        Ok(())
    );
}

#[test]
fn a_table_nobody_sits_at_takes_no_count() {
    let ctx = waiter();
    for state in [TableState::Free, TableState::NeedsCleaning] {
        assert!(
            matches!(
                decide_guest_count(state, 3, &ctx),
                Err(DomainError::Transition(error)) if error.machine == "table" && error.trigger == "count_guests"
            ),
            "{state:?}"
        );
    }
}

#[test]
fn a_count_names_at_least_one_guest() {
    let ctx = waiter();
    assert_eq!(
        decide_guest_count(TableState::Occupied, 0, &ctx),
        Err(DomainError::Empty {
            what: "guest count"
        })
    );
}

#[test]
fn it_needs_the_seats_permission_and_table_service() {
    let no_permission = ctx(
        PermissionSet::EMPTY,
        CapabilityContext::NONE.with(Capability::Tables),
    );
    assert_eq!(
        decide_guest_count(TableState::Occupied, 2, &no_permission),
        Err(DomainError::PermissionDenied {
            permission: "sales.table.manage"
        })
    );
    let no_tables = ctx(
        PermissionSet::EMPTY.with(Permission::ManageTables),
        CapabilityContext::NONE,
    );
    assert!(matches!(
        decide_guest_count(TableState::Occupied, 2, &no_tables),
        Err(DomainError::CapabilityDisabled { .. })
    ));
}
