// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The staff-confirmation hold, enforced
//! ([ADR-0116](../../../docs/adr/0116-the-qr-hold-is-derived-and-it-gates-firing.md)).
//!
//! ADR-0012 calls staff confirmation the protection on a printed QR code and
//! [ADR-0057](../../../docs/adr/0057-qr-ordering.md) defaults it on;
//! `OrderAcceptance::awaiting_staff_confirmation` says it is *"what stops a passer-by ordering forty
//! pizzas to a table they are not sitting at."* Before ADR-0116 the flag had three readers and all
//! three only **reported** it — nothing on the fire path consulted it, so a guest's order reached
//! the kitchen the moment anyone pressed fire. These cases are the enforcement: the refusal, the two
//! ways out of it, and the two ways the hold can end without anyone pressing anything.

use std::sync::Arc;

use pos_core::decision::Actor;
use pos_edge::{AppError, Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::ids::{
    DeviceId, EmployeeId, MenuItemId, OrderId, ReasonCodeId, StationId, StoreId, TableId,
    TaxClassId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{PublishedReasonCodes, ReasonAction};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{Open, OrderLineState, SalesChannel};

fn actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn item() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

/// The station the caller names as a fallback: the bootstrap session publishes no station plan, so
/// without one a released line is `UnroutableLine` and the test would be measuring routing rather
/// than the hold (ADR-0072's never-blank fallback).
fn station() -> StationId {
    StationId::new(Ulid::from_u128(900))
}

fn class() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

/// One already-priced line, in the shape the intake path hands to `open_inbound_order`.
fn a_priced_line() -> pos_core::menu::PricedLine {
    pos_core::menu::PricedLine {
        menu_item_id: item(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        modifier_menu_item_ids: Vec::new(),
        repriced: false,
    }
}

/// A session that can price and tax a QR order at a table, with the hold on the default (on).
fn session() -> EdgeSession {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class(),
    ));
    let rates = TaxRateTable::new().with(class(), SalesChannel::Qr, TaxRate::from_percent(10));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
}

fn edge_over(store: FakeStore, session: EdgeSession) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

/// Opens a guest's QR order at a table and returns the edge, the order, and its one line.
async fn a_guest_order(edge: &Edge<FakeStore>) -> (OrderId, pos_proto::ids::OrderLineId) {
    let opened = edge
        .open_inbound_order(
            actor().device_id,
            Open::from_known(SalesChannel::Qr),
            Some(table()),
            &[(a_priced_line(), false)],
            None,
        )
        .await
        .expect("a guest order opens");
    let line_id = edge
        .order_line_ids(opened.order_id)
        .first()
        .copied()
        .expect("the order has its line");
    (opened.order_id, line_id)
}

/// The framework default reason for refusing an order — `WRONG_ITEM`, which declares
/// `REASON_ACTION_REJECT_ORDER`.
fn a_reject_reason() -> ReasonCodeId {
    PublishedReasonCodes::framework_default()
        .for_action(ReasonAction::RejectOrder)
        .next()
        .expect("the framework set covers rejecting an order")
        .id
}

#[test]
fn a_held_order_cannot_be_fired() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        let (order_id, line_id) = a_guest_order(&edge).await;

        assert!(
            edge.awaits_staff_confirmation(order_id),
            "a guest's tabled QR order is held on the default policy"
        );
        let refused = edge.fire_line(actor(), line_id, Some(station())).await;
        assert!(
            matches!(refused, Err(AppError::AwaitingStaffConfirmation)),
            "the kitchen must not see it before a member of staff agrees, got {refused:?}"
        );
    });
}

#[test]
fn confirming_releases_it_to_the_kitchen() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        let (order_id, line_id) = a_guest_order(&edge).await;

        edge.confirm_inbound_order(actor(), order_id)
            .await
            .expect("staff confirm the order");

        assert!(
            !edge.awaits_staff_confirmation(order_id),
            "the hold has ended"
        );
        let fired = edge
            .fire_line(actor(), line_id, Some(station()))
            .await
            .expect("and the line fires");
        assert_eq!(fired.state, OrderLineState::Fired);
    });
}

#[test]
fn a_second_decision_is_refused_rather_than_recorded_twice() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        let (order_id, _) = a_guest_order(&edge).await;

        edge.confirm_inbound_order(actor(), order_id)
            .await
            .expect("the first confirmation lands");
        let again = edge.confirm_inbound_order(actor(), order_id).await;
        assert!(
            matches!(again, Err(AppError::NotAwaitingStaffConfirmation)),
            "two devices racing produce one decision and one audit entry, got {again:?}"
        );

        // And a rejection after a confirmation is the same refusal: the decision is made.
        let rejected = edge
            .reject_inbound_order(actor(), order_id, a_reject_reason())
            .await;
        assert!(matches!(
            rejected,
            Err(AppError::NotAwaitingStaffConfirmation)
        ));
    });
}

#[test]
fn rejecting_needs_a_reason_the_list_holds_for_rejecting() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        let (order_id, _) = a_guest_order(&edge).await;

        // An id no list holds. This is the check that makes the event's own contract
        // ("the mandatory reason, from the cloud-managed list") true rather than aspirational.
        let invented = ReasonCodeId::new(Ulid::from_u128(999_999));
        let refused = edge.reject_inbound_order(actor(), order_id, invented).await;
        assert!(
            matches!(refused, Err(AppError::ReasonCodeNotValid)),
            "an invented reason is refused, got {refused:?}"
        );

        // A reason the list holds, but for a different action: MAKING_CHANGE is a drawer reason.
        let drawer_reason = PublishedReasonCodes::framework_default()
            .for_action(ReasonAction::DrawerOpen)
            .find(|code| !code.declares(ReasonAction::RejectOrder))
            .expect("a drawer reason that is not a rejection reason")
            .id;
        let wrong_action = edge
            .reject_inbound_order(actor(), order_id, drawer_reason)
            .await;
        assert!(
            matches!(wrong_action, Err(AppError::ReasonCodeNotValid)),
            "a reason valid for another action is refused too, got {wrong_action:?}"
        );

        assert!(
            edge.awaits_staff_confirmation(order_id),
            "and a refused rejection leaves the order exactly where it was"
        );
    });
}

#[test]
fn rejecting_ends_the_order() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        let (order_id, line_id) = a_guest_order(&edge).await;

        edge.reject_inbound_order(actor(), order_id, a_reject_reason())
            .await
            .expect("staff refuse the order");

        assert!(
            !edge.awaits_staff_confirmation(order_id),
            "the decision is made, so it is no longer waiting"
        );
        // A refused order is not a released order: its lines still cannot be fired, because the
        // hold ended by refusal rather than by agreement.
        let refused = edge.fire_line(actor(), line_id, Some(station())).await;
        assert!(
            matches!(refused, Err(AppError::OrderRejected)),
            "a refused order's line does not become fireable, and the refusal says why rather \
             than reporting it as still waiting, got {refused:?}"
        );
        assert!(
            !edge
                .orders_awaiting_staff_confirmation()
                .contains(&order_id),
            "and it is off the queue"
        );
    });
}

#[test]
fn turning_the_policy_off_releases_every_held_order_at_once() {
    run_ready(async {
        // The behaviour PR #234 established for the *reported* answer, now the same answer for the
        // *enforced* one — because ADR-0116 made them one predicate rather than two flags.
        let edge = edge_over(FakeStore::default(), session());
        let (order_id, line_id) = a_guest_order(&edge).await;
        assert!(edge.awaits_staff_confirmation(order_id));

        let mut relaxed = session();
        relaxed.qr_staff_confirmation_required = false;
        edge.apply_session(relaxed);

        assert!(
            !edge.awaits_staff_confirmation(order_id),
            "no per-order clearing step: the policy is the input, so switching it off releases \
             every held order on the next read"
        );
        edge.fire_line(actor(), line_id, Some(station()))
            .await
            .expect("and the line fires");
    });
}

#[test]
fn the_hold_survives_a_restart() {
    run_ready(async {
        let store = FakeStore::default();

        // Session one: a guest's order arrives and is held. Nothing is confirmed.
        let order_id = {
            let edge = edge_over(store.clone(), session());
            let (order_id, _) = a_guest_order(&edge).await;
            order_id
        };

        // Session two over the same log — the restart. The hold is derived from
        // `sales.order.opened`'s channel and table plus the live policy, so it comes back with no
        // new table and no new column (ADR-0116).
        let edge = edge_over(store.clone(), session());
        edge.rebuild().await.expect("rebuilds from the log");
        assert!(
            edge.awaits_staff_confirmation(order_id),
            "a restart must not release a guest's order"
        );

        // And a confirmation recorded before a restart is honoured after one.
        edge.confirm_inbound_order(actor(), order_id)
            .await
            .expect("staff confirm it");
        let after = edge_over(store.clone(), session());
        after.rebuild().await.expect("rebuilds again");
        assert!(
            !after.awaits_staff_confirmation(order_id),
            "and must not re-hold an order staff have already released"
        );
    });
}

#[test]
fn the_queue_lists_only_what_is_waiting() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        let (first, _) = a_guest_order(&edge).await;
        let (second, _) = a_guest_order(&edge).await;

        let waiting = edge.orders_awaiting_staff_confirmation();
        assert_eq!(waiting.len(), 2, "both guest orders are waiting");
        assert!(waiting.contains(&first) && waiting.contains(&second));
        assert!(
            waiting[0] < waiting[1],
            "oldest first — ULIDs sort by mint time, so arrival order needs no timestamp"
        );

        edge.confirm_inbound_order(actor(), first)
            .await
            .expect("one is confirmed");
        assert_eq!(
            edge.orders_awaiting_staff_confirmation(),
            vec![second],
            "and it leaves the queue"
        );
    });
}
