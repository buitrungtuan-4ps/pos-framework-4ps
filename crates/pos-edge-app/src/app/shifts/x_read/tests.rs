// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A shift's X and the store's: the figures, who sees the expected cash, and that reading changes
//! nothing (ADR-0187 decision 4).

use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_core::permission::{Permission, PermissionSet};
use pos_fakes::FakeStore;
use pos_proto::ids::{EmployeeId, TableId};
use pos_proto::ulid::Ulid;
use pos_proto::{PaymentMethod, ShiftState};

use crate::app::tests::{a_line, actor, cash, identity, vnd};
use crate::app::{Edge, EdgeSession, StaffAuth, StaffRoster};
use crate::receipt::InMemoryReceipts;

/// The actor sees takings only; `MGR-1` (employee 11) also reads the X's expected cash.
fn edge() -> Edge<FakeStore> {
    let mut staff = StaffRoster::new();
    for (code, id, permissions) in [
        (
            "CASH-1",
            10,
            PermissionSet::EMPTY.with(Permission::ViewTakings),
        ),
        (
            "MGR-1",
            11,
            PermissionSet::EMPTY.with(Permission::ReadShiftX),
        ),
    ] {
        staff.insert(
            code,
            StaffAuth {
                employee_id: Some(EmployeeId::new(Ulid::from_u128(id))),
                permissions,
                permissions_with_approval: PermissionSet::EMPTY,
                discount_ceiling: None,
                pin_phc: None,
            },
        );
    }
    let mut session = EdgeSession::bootstrap();
    session.staff = staff;
    Edge::new(
        FakeStore::default(),
        identity(),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds")
}

fn manager() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(11)),
        ..actor()
    }
}

/// Sells one 165,000 bill on `table`, paid by `payment`.
async fn sell(edge: &Edge<FakeStore>, table: u128, payment: Payment) {
    let table = TableId::new(Ulid::from_u128(table));
    edge.seat_table(actor(), table, None).await.expect("seats");
    edge.add_line(actor(), table, a_line()).await.expect("adds");
    let bill = edge.open_bill(actor(), table).await.expect("opens a bill");
    edge.settle_bill(actor(), bill.bill_id, vec![payment], None)
        .await
        .expect("settles");
}

#[test]
fn an_x_reads_the_shift_as_it_stands_and_resets_nothing() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        assert!(edge.reads_x(actor()) && edge.reads_x(manager()));
        let opened = edge.open_shift(actor(), vnd(500_000)).await.expect("opens");
        sell(&edge, 400, cash(165_000)).await;
        let card = Payment {
            method: PaymentMethod::Card,
            tendered: vnd(165_000),
            applied_to_bill: vnd(165_000),
            tip: vnd(0),
        };
        sell(&edge, 401, card).await;

        let x = edge.shift_x(manager(), opened.shift_id).expect("reads");
        assert_eq!(x.state, ShiftState::Open);
        assert_eq!((x.figures.sales, x.figures.bills), (vnd(330_000), 2));
        assert_eq!(x.figures.tenders.cash, vnd(165_000));
        assert_eq!(x.figures.tenders.card, vnd(165_000));
        assert_eq!(x.figures.tenders.qr, vnd(0));
        assert_eq!(x.figures.expected_cash, Some(vnd(665_000)));
        assert_eq!(
            edge.shift_x(manager(), opened.shift_id)
                .expect("reads again"),
            x,
            "reading again reads the same"
        );
        // A blind-close store keeps the expectation from anybody without `reports.shift.read_x`.
        let blind = edge.shift_x(actor(), opened.shift_id).expect("reads");
        assert_eq!(blind.figures.expected_cash, None);
        assert_eq!(blind.figures.sales, vnd(330_000));

        let store = edge.store_x(manager()).expect("reads the store");
        assert!(!store.all_closed);
        assert_eq!(store.shifts.len(), 1);

        // The close finds every figure where the X left it.
        edge.count_shift(actor(), opened.shift_id, 665_000)
            .await
            .expect("counts");
        let closed = edge
            .close_shift(actor(), opened.shift_id)
            .await
            .expect("closes");
        assert_eq!(closed.expected_amount, Some(vnd(665_000)));
        assert_eq!(closed.variance, Some(vnd(0)));

        // Every drawer closed: the store's X is the day's closed shifts, its end of day.
        let day = edge.store_x(manager()).expect("reads the day");
        assert!(day.all_closed);
        assert_eq!(day.shifts[0].state, ShiftState::Closed);
        let totals = day.totals.expect("the day's totals");
        assert_eq!(totals.sales, vnd(330_000));
        assert_eq!(totals.expected_cash, Some(vnd(665_000)));
    });
}

#[test]
fn a_store_with_no_shift_today_reads_none_and_is_not_all_closed() {
    let edge = edge();
    let store = edge.store_x(manager()).expect("reads");
    assert!(!store.all_closed);
    assert!(store.shifts.is_empty() && store.totals.is_none());
}
