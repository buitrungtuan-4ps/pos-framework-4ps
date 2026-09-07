// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `EdgeOrderIn` against the shared `OrderIn` contract suite ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)).
//!
//! The suite is the specification of what a caller may rely on ([ADR-0026](../../../docs/adr/0026-port-shapes.md)
//! §5): a first submit creates the order and its total comes from the store's own menu; a retry on
//! the same `(channel, reference)` returns the same order with `created: false`; the same reference
//! on two channels is two orders; a quoted price that differs is repriced, not honoured; an unknown
//! item and an empty order are refused. Running it here proves the edge's implementation honours all
//! of that, against the in-memory store and ledger.

use std::sync::Arc;

use pos_contract_tests::harness::{OrderInHarness, Setup};
use pos_edge::{
    Edge, EdgeOrderIn, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, MenuItemId, StoreId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

fn store() -> StoreId {
    StoreId::new(Ulid::from_u128(7))
}

fn price() -> Money {
    Money::new(CurrencyCode::VND, 120_000)
}

/// The catalog and rate table a fresh edge is seeded with: one item the store sells at [`price`], on
/// the standard class, taxed on every channel the suite exercises.
fn seeded_session() -> EdgeSession {
    let (item, unit_price) = (MenuItemId::new(Ulid::from_u128(500)), price());
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item,
        DisplayName::new("Margherita"),
        unit_price,
        class,
    ));
    let rates = TaxRateTable::new()
        .with(class, SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class, SalesChannel::Takeaway, TaxRate::from_percent(10))
        .with(class, SalesChannel::Delivery, TaxRate::from_percent(10))
        // QR joined this table when the staff-confirmation cases started using the channel a
        // guest's order actually arrives on (ADR-0116). Without it intake refuses the order for
        // want of a rate, which is the correct refusal and the wrong test.
        .with(class, SalesChannel::Qr, TaxRate::from_percent(10));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
}

/// Supplies a fresh `EdgeOrderIn` — a new edge over the in-memory store and a new idempotency
/// ledger — plus the known/unknown menu items and the store id the cases use.
struct EdgeIntakeHarness;

impl OrderInHarness for EdgeIntakeHarness {
    type Intake = EdgeOrderIn<FakeStore, InMemoryQueueNumbers>;

    async fn fresh(&self) -> Setup<Self::Intake> {
        let edge = Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store()),
            seeded_session(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator");
        Ok(EdgeOrderIn::new(
            Arc::new(edge),
            InMemoryQueueNumbers::new(),
            DeviceId::new(Ulid::from_u128(20)),
        ))
    }

    fn known_menu_item(&self) -> (MenuItemId, Money) {
        (MenuItemId::new(Ulid::from_u128(500)), price())
    }

    fn unknown_menu_item(&self) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(u128::MAX))
    }

    fn store_id(&self) -> StoreId {
        store()
    }
}

mod order_in {
    use super::{EdgeIntakeHarness, run_ready};
    pos_contract_tests::order_in_suite!(EdgeIntakeHarness, run_ready);
}

/// The staff hold is ONE question, answered once — and "does it get a queue number" is a DIFFERENT
/// question that must not be derived from it.
///
/// Regression. Both were once asked twice and in two ways, and the disagreement was invisible
/// because it only showed on a store that had switched the hold off:
///
/// * the durable ledger row was written as `table_id.is_some()` alone, while the acceptance
///   returned to the cloud was `table_id.is_some() && qr_staff_confirmation_required` — so with the
///   hold off, the first delivery answered "not waiting" and every replay of the same
///   at-least-once reference answered "waiting", stranding the guest's order behind a hold the
///   store had turned off (and there is no confirm route yet to release it);
/// * the replay path chose the queue number with `if awaiting { None } else { allocate() }`, which
///   only ever worked because the stored flag happened to equal `table_id.is_some()`. Correct that
///   flag and the else-branch allocates — putting a queue number on a floor order that must never
///   have one, silently, because `allocate_queue_number` is idempotent rather than fussy.
///
/// So the cases below fix a table order on a hold-off store, which is the one shape that
/// distinguishes all three predicates, and pin first-answer == replay-answer for both facts.
mod staff_confirmation {
    use std::sync::Arc;

    use pos_edge::{Edge, EdgeOrderIn, InMemoryQueueNumbers, InMemoryReceipts, StoreIdentity};
    use pos_fakes::FakeStore;
    use pos_fakes::executor::run_ready;
    use pos_ports::order_in::{
        ExternalReference, InboundOrder, InboundOrderLine, OrderAcceptance, OrderIn,
    };
    use pos_proto::SalesChannel;
    use pos_proto::ids::{DeviceId, MenuItemId, TableId};
    use pos_proto::quantity::Quantity;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    use super::{seeded_session, store};

    /// An edge whose store has published a `qr` node turning the staff hold OFF (ADR-0080, M7).
    fn intake_without_the_hold() -> EdgeOrderIn<FakeStore, InMemoryQueueNumbers> {
        let mut session = seeded_session();
        session.qr_staff_confirmation_required = false;
        let edge = Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store()),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator");
        EdgeOrderIn::new(
            Arc::new(edge),
            InMemoryQueueNumbers::new(),
            DeviceId::new(Ulid::from_u128(20)),
        )
    }

    /// An edge on the default policy: the hold is ON, as ADR-0012 and ADR-0057 require.
    fn intake_with_the_hold() -> EdgeOrderIn<FakeStore, InMemoryQueueNumbers> {
        let edge = Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store()),
            seeded_session(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator");
        EdgeOrderIn::new(
            Arc::new(edge),
            InMemoryQueueNumbers::new(),
            DeviceId::new(Ulid::from_u128(20)),
        )
    }

    /// A QR order: on the QR channel, and it names a table, so it is served there rather than
    /// called back by number.
    ///
    /// The channel used to be `DineIn` here, which was wrong in a way that only became visible
    /// once the channel joined the hold predicate (ADR-0116): the fixture claimed to be a QR
    /// order, the assertions below claim to be about the QR hold, and a `DineIn` order is not
    /// held at all — so all three tests would have passed against a predicate that did nothing.
    fn at_a_table(reference: &str) -> InboundOrder {
        InboundOrder {
            external_reference: ExternalReference::parse(reference).expect("a valid reference"),
            sales_channel: Open::from_known(SalesChannel::Qr),
            store_id: store(),
            table_id: Some(TableId::new(Ulid::from_u128(42))),
            subject_id: None,
            lines: vec![InboundOrderLine {
                menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
                quantity: Quantity::ONE,
                modifier_menu_item_ids: Vec::new(),
                quoted_unit_price: None,
                note: None,
            }],
            placed_at: pos_contract_tests::fixtures::instant(),
        }
    }

    fn submit(
        intake: &EdgeOrderIn<FakeStore, InMemoryQueueNumbers>,
        order: &InboundOrder,
    ) -> OrderAcceptance {
        run_ready(intake.submit(order)).expect("the store accepts the order")
    }

    #[test]
    fn the_hold_answer_survives_a_replay_of_the_same_reference() {
        let intake = intake_without_the_hold();
        let order = at_a_table("QR-1");

        let first = submit(&intake, &order);
        let replay = submit(&intake, &order);

        assert!(first.created, "the first delivery creates the order");
        assert!(!replay.created, "the second is recognised as a repeat");
        assert_eq!(
            first.order_id, replay.order_id,
            "a repeat resolves to the same order"
        );
        assert!(
            !first.awaiting_staff_confirmation,
            "the store switched the hold off, so nothing waits for staff"
        );
        assert_eq!(
            first.awaiting_staff_confirmation, replay.awaiting_staff_confirmation,
            "the replay must answer what the first delivery answered — the relay is \
             at-least-once, and an order that flips into a hold on retry can never be released"
        );
    }

    #[test]
    fn a_table_order_gets_no_queue_number_on_either_path() {
        let intake = intake_without_the_hold();
        let order = at_a_table("QR-2");

        let first = submit(&intake, &order);
        let replay = submit(&intake, &order);

        assert_eq!(
            first.queue_number, None,
            "a table order is served where it sits; a queue number is for a call-back"
        );
        assert_eq!(
            replay.queue_number, None,
            "and the replay must not mint one — the hold being off is not permission to allocate"
        );
    }

    #[test]
    fn a_tableless_order_keeps_its_number_across_a_replay() {
        let intake = intake_without_the_hold();
        let order = InboundOrder {
            table_id: None,
            sales_channel: Open::from_known(SalesChannel::Takeaway),
            ..at_a_table("TA-1")
        };

        let first = submit(&intake, &order);
        let replay = submit(&intake, &order);

        assert!(
            first.queue_number.is_some(),
            "a takeaway order is called back by number, so it is given one"
        );
        assert_eq!(
            first.queue_number, replay.queue_number,
            "and the replay shouts the same number rather than burning a second one"
        );
    }

    #[test]
    fn a_tabled_qr_order_is_held_when_the_policy_is_on() {
        // The default posture, and the one ADR-0012 calls the protection on a printed code.
        let intake = intake_with_the_hold();
        let accepted = submit(&intake, &at_a_table("QR-HOLD"));

        assert!(
            accepted.awaiting_staff_confirmation,
            "a guest's tabled QR order waits for staff on the default policy"
        );
    }

    #[test]
    fn a_tabled_order_on_another_channel_is_not_a_guest_submission() {
        // The distinction the channel argument buys (ADR-0116). A marketplace order that happens
        // to name a table is not somebody scanning a printed code, and holding it would leave a
        // delivery rider waiting for a confirmation nobody knows to give.
        let intake = intake_with_the_hold();
        let order = InboundOrder {
            sales_channel: Open::from_known(SalesChannel::Delivery),
            ..at_a_table("DL-1")
        };
        let accepted = submit(&intake, &order);

        assert!(
            !accepted.awaiting_staff_confirmation,
            "only a QR submission is held; a tabled delivery order is not"
        );
    }

    #[test]
    fn a_tableless_qr_order_is_not_held() {
        // A QR order taken at the counter names no table, so there is no "table they are not
        // sitting at" to protect — and it is called back by number instead.
        let intake = intake_with_the_hold();
        let order = InboundOrder {
            table_id: None,
            ..at_a_table("QR-COUNTER")
        };
        let accepted = submit(&intake, &order);

        assert!(
            !accepted.awaiting_staff_confirmation,
            "a tableless QR order has no table to protect, so it is not held"
        );
        assert!(
            accepted.queue_number.is_some(),
            "and it is called back by number"
        );
    }
}
