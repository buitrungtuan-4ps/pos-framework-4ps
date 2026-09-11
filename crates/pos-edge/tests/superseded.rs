// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A box a replacement has taken opens nothing new, and finishes everything it holds
//! ([ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md), closing
//! [ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)'s first named gap).
//!
//! The refusal here is the most dangerous one in the tree: ADR-0108 declined to build it for a
//! year because "a shop that cannot take money is a worse outcome than a shop running last week's
//! binary". So the load-bearing half of this suite is not the three refusals — it is the much
//! longer list of things that must **still work** on a superseded box, and the two standings that
//! must not trigger it at all.
//!
//! Four properties, each of them a way the gate could look finished and be wrong:
//!
//!   * a table seated *before* the supersession takes another round, fires it, is billed and
//!     settles — on the box that will not seat a new one. A gate that stopped that would strand
//!     guests who are already eating, on a machine whose event log is the only place their order
//!     exists;
//!   * `Invalid` keeps selling. Under take-once its likeliest cause is a config rollback moving the
//!     published node backwards, which reads `Invalid` on *every* till in the shop at once — a gate
//!     that fired on it would turn an admin clicking "roll back" into a dark shop;
//!   * a lease that cannot be read changes nothing, in either direction. Not down (a SQLite hiccup
//!     must not stop a shop trading) and not up (a box a replacement has taken must not
//!     un-supersede itself because a read failed);
//!   * a channel order refused by a superseded box comes back `unavailable`, not `internal`, so the
//!     relay leaves it queued for the replacement instead of failing an order a guest has paid for.

use core::future::Future;
use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::lease::{LeaseGeneration, LeaseStanding};
use pos_edge::{
    AppError, Edge, EdgeOrderIn, EdgeSession, HeldLease, InMemoryLease, InMemoryQueueNumbers,
    InMemoryReceipts, LeaseWatch, StoreIdentity, StoreLease,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::PortError;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::order_in::{ExternalReference, InboundOrder, InboundOrderLine, OrderIn};
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StationId, StoreId, TableId, TaxClassId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{BillState, Open, PaymentMethod, SalesChannel};

use pos_core::billing::Payment;
use pos_core::decision::Actor;

fn server() -> Actor {
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

fn other_table() -> TableId {
    TableId::new(Ulid::from_u128(43))
}

fn station() -> StationId {
    StationId::new(Ulid::from_u128(900))
}

fn class() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

fn store() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn generation(value: u64) -> LeaseGeneration {
    LeaseGeneration::new(value)
}

/// A store that can price and tax one dine-in line. Nothing here is about the lease.
fn session() -> EdgeSession {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class(),
    ));
    // Both channels, because the relay test below submits a takeaway order and an unpriceable
    // line would be refused for the wrong reason — `failed_precondition` from the repricer rather
    // than the `unavailable` this suite is about.
    let rates = TaxRateTable::new()
        .with(class(), SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class(), SalesChannel::Takeaway, TaxRate::from_percent(10));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
}

fn edge() -> Edge<FakeStore> {
    Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(store()),
        session(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

fn a_line() -> pos_edge::LineDraft {
    pos_edge::LineDraft {
        menu_item_id: item(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// Drives this box's standing to `Superseded` the way production does: the box takes generation 4
/// on first sight, and a later pull publishes 5.
///
/// Deliberately *not* `edge.lease().set(...)` — that would prove the gate reads a field somebody
/// set, which is not the property. The property is that the real take-once authority, weighed
/// against the real published generation, produces a box that will not seat a table.
async fn supersede(edge: &Edge<FakeStore>) -> Arc<LeaseWatch> {
    let watch = watch_over(edge, InMemoryLease::new());
    watch.settle(Some(generation(4))).await;
    assert_eq!(
        edge.lease().get(),
        LeaseStanding::Active,
        "the first sight of a generation is this box's own"
    );
    watch.settle(Some(generation(5))).await;
    assert_eq!(edge.lease().get(), LeaseStanding::Superseded);
    watch
}

fn watch_over<A>(edge: &Edge<FakeStore>, authority: A) -> Arc<LeaseWatch>
where
    A: pos_edge::LeaseAuthority + 'static,
{
    Arc::new(LeaseWatch::new(
        Arc::new(StoreLease::new(authority, store())),
        Arc::clone(edge.lease()),
    ))
}

#[test]
fn a_replaced_box_will_not_open_a_shift_a_table_or_a_channel_order() {
    run_ready(async {
        let edge = edge();
        supersede(&edge).await;

        assert!(
            matches!(
                edge.open_shift(server(), vnd(500_000)).await,
                Err(AppError::Superseded)
            ),
            "a replaced machine does not open a new trading day"
        );
        assert!(
            matches!(
                edge.seat_table(server(), table(), None).await,
                Err(AppError::Superseded)
            ),
            "a replaced machine seats nobody new"
        );
        let refused = edge
            .open_inbound_order(
                server().device_id,
                Open::from_known(SalesChannel::Takeaway),
                None,
                &[],
                None,
            )
            .await;
        assert!(
            matches!(refused, Err(AppError::Superseded)),
            "and it takes no new channel order, so the relay can leave it for the replacement"
        );

        // Nothing was written. A refusal that had already appended an event would leave the
        // replacement and this box each holding half a shop's history.
        let query = EventQuery::first(store(), NonZeroU32::new(100).expect("a positive limit"));
        let written = edge.store().read(&query).await.expect("read the log");
        assert!(
            written.is_empty(),
            "the refusal happens before any event is minted"
        );
    });
}

#[test]
fn a_table_seated_before_the_replacement_still_eats_pays_and_leaves() {
    run_ready(async {
        // The whole shape of ADR-0123 in one case: the floor **drains**. This box is the store when
        // the guests sit down, and is not by the time they order their second round.
        let edge = edge();
        let shift = edge
            .open_shift(server(), vnd(500_000))
            .await
            .expect("open the shift while this box is still the store");
        edge.seat_table(server(), table(), None)
            .await
            .expect("seat them");
        let first = edge
            .add_line(server(), table(), a_line())
            .await
            .expect("their first round");
        edge.fire_line(server(), first.order_line_id, Some(station()))
            .await
            .expect("fire it");

        // A replacement is activated mid-service.
        supersede(&edge).await;

        // Everything that finishes this table still works.
        let second = edge
            .add_line(server(), table(), a_line())
            .await
            .expect("guests order in waves; the second round is not new work");
        edge.fire_line(server(), second.order_line_id, Some(station()))
            .await
            .expect("the kitchen keeps making it");
        let bill = edge
            .open_bill(server(), table())
            .await
            .expect("the check still comes");
        // What the guests owe, from the same read the till shows them — `total_due` on the view is
        // only filled in once the bill has settled.
        let due = edge
            .check_totals(table())
            .expect("the check adds up")
            .total_due;
        let settled = edge
            .settle_bill(
                server(),
                bill.bill_id,
                vec![Payment {
                    method: PaymentMethod::Cash,
                    tendered: due,
                    applied_to_bill: due,
                    tip: vnd(0),
                }],
                None,
            )
            .await
            .expect("a shop that cannot take money is the outcome ADR-0108 refused to ship");
        assert_eq!(settled.state, BillState::Settled);
        edge.clean_table(server(), table())
            .await
            .expect("and the table is cleared");
        edge.count_shift(server(), shift.shift_id, 500_000)
            .await
            .expect("the count still runs");
        edge.close_shift(server(), shift.shift_id)
            .await
            .expect("and the day still closes");

        // …but the floor does not refill.
        assert!(
            matches!(
                edge.seat_table(server(), other_table(), None).await,
                Err(AppError::Superseded)
            ),
            "a cleared table cannot be seated again on a box that has been replaced"
        );
    });
}

#[test]
fn a_box_ahead_of_the_cloud_keeps_selling() {
    run_ready(async {
        // `Invalid` under take-once is most plausibly a config rollback, which every till in the
        // shop reads at once. Refusing on it would convert an admin's rollback into a dark shop —
        // so this is the assertion that stops a future "tidy the two standings up" change.
        let edge = edge();
        let watch = watch_over(&edge, InMemoryLease::new());
        watch.settle(Some(generation(5))).await;
        watch.settle(Some(generation(3))).await;
        assert_eq!(edge.lease().get(), LeaseStanding::Invalid);

        edge.seat_table(server(), table(), None)
            .await
            .expect("a box ahead of its authority is a thing to look at, not a shop to close");
        assert_eq!(
            edge.lease().token(),
            "invalid",
            "and it says so on every answer"
        );
    });
}

#[test]
fn a_store_that_was_never_issued_a_lease_is_untouched() {
    run_ready(async {
        // Every store in the fleet today. The refusal begins the day an operator issues a lease and
        // a replacement takes it, not the day this shipped.
        let edge = edge();
        let watch = watch_over(&edge, InMemoryLease::new());
        watch.settle(None).await;
        assert_eq!(edge.lease().get(), LeaseStanding::Active);
        assert_eq!(edge.lease().token(), "active");
        edge.seat_table(server(), table(), None)
            .await
            .expect("unchanged behaviour for a store with no lease");
    });
}

/// A lease that answers nothing but an error — the SQLite hiccup, the locked file.
struct UnreadableLease;

impl HeldLease for UnreadableLease {
    fn weigh(
        &self,
        _published: Option<LeaseGeneration>,
    ) -> core::pin::Pin<Box<dyn Future<Output = Result<LeaseStanding, PortError>> + Send + '_>>
    {
        Box::pin(async { Err(unreadable()) })
    }

    fn forget(
        &self,
    ) -> core::pin::Pin<Box<dyn Future<Output = Result<(), PortError>> + Send + '_>> {
        Box::pin(async { Err(unreadable()) })
    }
}

fn unreadable() -> PortError {
    PortError::unavailable(
        pos_ports::PortName::EventStore,
        "the store could not be read",
    )
}

#[test]
fn a_lease_that_cannot_be_read_changes_nothing_in_either_direction() {
    run_ready(async {
        // Down: a hiccup must not stop a shop seating tables.
        let trading = edge();
        let unreadable = Arc::new(LeaseWatch::new(
            Arc::new(UnreadableLease),
            Arc::clone(trading.lease()),
        ));
        unreadable.settle(Some(generation(9))).await;
        assert_eq!(trading.lease().get(), LeaseStanding::Active);
        trading
            .seat_table(server(), table(), None)
            .await
            .expect("an unreadable lease is not a reason to refuse a guest");

        // Up: and it must not un-supersede a box a replacement has already taken. This is the half
        // that would be silently wrong if `settle` cleared the cell on error.
        let replaced = edge();
        supersede(&replaced).await;
        let unreadable = Arc::new(LeaseWatch::new(
            Arc::new(UnreadableLease),
            Arc::clone(replaced.lease()),
        ));
        unreadable.settle(Some(generation(5))).await;
        assert_eq!(
            replaced.lease().get(),
            LeaseStanding::Superseded,
            "a failed read must not promote a replaced machine back to being the store"
        );
    });
}

#[test]
fn a_channel_order_is_left_queued_for_the_replacement_not_failed() {
    run_ready(async {
        // The relay pulls orders from the cloud and acknowledges what it accepted. A superseded box
        // must leave one **queued**, so the replacement gets it — and the way it says so is the
        // `PortError` status, because that is all the relay sees. `Unavailable` means "not from
        // here, ask again"; `Internal` — which the catch-all in `port_error_from_app` would have
        // produced — reads as a bug in this store and invites the caller to give up on an order a
        // guest has already paid a marketplace for.
        let edge = Arc::new(edge());
        supersede(&edge).await;
        let intake = EdgeOrderIn::new(
            Arc::clone(&edge),
            InMemoryQueueNumbers::new(),
            server().device_id,
        );

        let order = InboundOrder {
            external_reference: ExternalReference::parse("MKT-1").expect("a valid reference"),
            sales_channel: Open::from_known(SalesChannel::Takeaway),
            store_id: store(),
            table_id: None,
            subject_id: None,
            lines: vec![InboundOrderLine {
                menu_item_id: item(),
                quantity: Quantity::ONE,
                modifier_menu_item_ids: Vec::new(),
                quoted_unit_price: None,
                note: None,
            }],
            placed_at: pos_contract_tests::fixtures::instant(),
        };
        let refused = intake
            .submit(&order)
            .await
            .expect_err("a replaced box takes nothing new");
        assert_eq!(
            refused.status(),
            ErrorStatus::Unavailable,
            "the relay must read this as `try the other box`, not as a failure to drop the order"
        );

        // And nothing was written, so the replacement is free to accept the same reference.
        let query = EventQuery::first(store(), NonZeroU32::new(100).expect("a positive limit"));
        assert!(
            edge.store()
                .read(&query)
                .await
                .expect("read the log")
                .is_empty(),
            "an order refused this way leaves no half-order behind"
        );
    });
}
