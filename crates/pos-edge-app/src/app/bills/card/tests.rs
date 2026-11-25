// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A card through the terminal (ADR-0230): a capture is a payment, a decline records nothing, and
//! an unknown result parks the bill until a person resolves it — across a restart too.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_fakes::{FakePaymentTerminal, FakeStore};
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::events::BillingPaymentCaptured;
use pos_proto::ids::{BillId, ReasonCodeId, TableId};
use pos_proto::money::Money;
use pos_proto::ulid::Ulid;
use pos_proto::{BillState, EventType, PaymentOutcome};

use crate::app::tests::{a_line, actor, cash, identity, vnd};
use crate::app::{
    AppError, CardAsk, CardOutcome, CardRefusal, CardResolution, Edge, EdgeSession, TillScope,
};
use crate::receipt::InMemoryReceipts;

fn edge_over(store: FakeStore, terminal: &FakePaymentTerminal) -> Edge<FakeStore> {
    Edge::new(
        store,
        identity(),
        EdgeSession::bootstrap(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds")
    .with_card_terminal(terminal.clone())
}

/// The bootstrap session with a manager, `MGR-1` (PIN `4417`), who may void a bill: the approver a
/// confirm by hand needs (ADR-0243).
fn with_a_manager() -> EdgeSession {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    use pos_core::permission::{Permission, PermissionSet};

    let pin_phc = Argon2::default()
        .hash_password_with_salt(b"4417", b"a-fixed-test-slt")
        .expect("hash")
        .to_string();
    let mut session = EdgeSession::bootstrap();
    session.staff.insert(
        "MGR-1",
        crate::app::StaffAuth {
            employee_id: Some(pos_proto::ids::EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::EMPTY.with(Permission::VoidBill),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(pin_phc),
        },
    );
    session
}

/// Seats a table, sells one line and opens its bill: the bill and what it owes.
async fn an_open_bill(edge: &Edge<FakeStore>, table: u128) -> (BillId, Money) {
    let table = TableId::new(Ulid::from_u128(table));
    edge.seat_table(actor(), table, None).await.expect("seats");
    edge.add_line(actor(), table, a_line()).await.expect("adds");
    let bill_id = edge.open_bill(actor(), table).await.expect("opens").bill_id;
    let owed = edge.bill_totals(bill_id).expect("assembles").total_due;
    (bill_id, owed)
}

fn ask(amount: Money) -> CardAsk {
    CardAsk {
        amount,
        tip: vnd(0),
        split_ways: None,
    }
}

async fn payments(store: &FakeStore) -> Vec<BillingPaymentCaptured> {
    store
        .read(&EventQuery::first(
            identity().store_id,
            NonZeroU32::new(1_000).expect("not zero"),
        ))
        .await
        .expect("reads")
        .iter()
        .filter(|envelope| envelope.event_type.known() == Some(EventType::BillingPaymentCaptured))
        .map(|envelope| envelope.data.decode().expect("decodes"))
        .collect()
}

fn bill_state(edge: &Edge<FakeStore>, bill_id: BillId) -> BillState {
    edge.lock_projection().bill(bill_id).expect("known").state
}

#[test]
fn a_captured_card_settles_the_bill_under_the_id_the_terminal_was_given() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let terminal = FakePaymentTerminal::new();
        let edge = edge_over(store.clone(), &terminal);
        let (bill_id, owed) = an_open_bill(&edge, 1).await;

        let outcome = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await
            .expect("takes the card");
        let CardOutcome::Captured { payment_id, taken } = outcome else {
            panic!("a staged capture is captured: {outcome:?}");
        };
        assert!(
            taken.settled.is_some(),
            "it covered the bill, so it settled it"
        );
        assert_eq!(bill_state(&edge, bill_id), BillState::Settled);
        assert_eq!(terminal.authorisation_count(), 1);

        let recorded = payments(&store).await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].payment_id, payment_id);
        assert_eq!(recorded[0].outcome.known(), PaymentOutcome::Captured);
        assert_eq!(
            recorded[0]
                .terminal_reference
                .as_ref()
                .map(|r| r.as_str().to_owned()),
            Some(format!("fake-{payment_id}"))
        );
    });
}

#[test]
fn a_declined_card_records_nothing_and_the_bill_stays_open() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let terminal = FakePaymentTerminal::new();
        let edge = edge_over(store.clone(), &terminal);
        let (bill_id, owed) = an_open_bill(&edge, 2).await;

        terminal.stage_outcome(PaymentOutcome::Declined);
        let outcome = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await
            .expect("asks the terminal");
        assert!(matches!(outcome, CardOutcome::Declined { .. }));
        assert!(payments(&store).await.is_empty());
        assert_eq!(bill_state(&edge, bill_id), BillState::Open);
        // Another tender is taken as before.
        edge.settle_bill(actor(), bill_id, vec![cash(owed.amount_minor)], None)
            .await
            .expect("settles in cash");
    });
}

#[test]
fn an_unknown_result_parks_the_bill_and_it_takes_nothing_else_even_after_a_restart() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let terminal = FakePaymentTerminal::new();
        let edge = edge_over(store.clone(), &terminal);
        let (bill_id, owed) = an_open_bill(&edge, 3).await;

        terminal.stage_outcome(PaymentOutcome::Unknown);
        let outcome = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await
            .expect("asks the terminal");
        let CardOutcome::Unknown { payment_id } = outcome else {
            panic!("an unknown result is never guessed: {outcome:?}");
        };
        let recorded = payments(&store).await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].outcome.known(), PaymentOutcome::Unknown);
        let order_id = edge
            .lock_projection()
            .bill(bill_id)
            .expect("known")
            .order_id;
        assert!(
            edge.paid_towards(order_id, Some(bill_id)).is_none(),
            "an unknown card pays nothing"
        );

        // Every act that would take money twice, or drop it, is refused.
        let refused = edge
            .settle_bill(actor(), bill_id, vec![cash(owed.amount_minor)], None)
            .await;
        assert!(
            matches!(refused, Err(AppError::Card(CardRefusal::ResultUnknown))),
            "{refused:?}"
        );
        let refused = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await;
        assert!(
            matches!(refused, Err(AppError::Card(CardRefusal::ResultUnknown))),
            "{refused:?}"
        );
        let refused = edge
            .void_bill_at(
                actor(),
                TillScope::default(),
                bill_id,
                ReasonCodeId::new(Ulid::from_u128(1)),
                None,
            )
            .await;
        assert!(
            matches!(refused, Err(AppError::Card(CardRefusal::ResultUnknown))),
            "{refused:?}"
        );

        // A restart reads the park back off the log.
        let restarted = edge_over(store.clone(), &terminal);
        restarted.rebuild().await.expect("rebuilds");
        let parked = restarted.parked_card(bill_id).expect("still parked");
        assert_eq!(parked.payment_id, payment_id);
        assert_eq!(parked.reference, format!("fake-{payment_id}"));
    });
}

#[test]
fn a_look_up_settles_a_card_the_acquirer_later_says_was_captured() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let terminal = FakePaymentTerminal::new();
        let edge = edge_over(store.clone(), &terminal);
        let (bill_id, owed) = an_open_bill(&edge, 4).await;
        terminal.stage_outcome(PaymentOutcome::Unknown);
        let CardOutcome::Unknown { payment_id } = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await
            .expect("asks")
        else {
            panic!("staged unknown");
        };

        // Still unknown: nothing changes.
        let still = edge
            .resolve_card_at(
                actor(),
                TillScope::default(),
                (bill_id, payment_id),
                CardResolution::LookUp,
            )
            .await
            .expect("looks up");
        assert!(matches!(still, CardOutcome::Unknown { .. }));
        assert!(edge.parked_card(bill_id).is_some());

        terminal.conclude(payment_id, PaymentOutcome::Captured);
        let resolved = edge
            .resolve_card_at(
                actor(),
                TillScope::default(),
                (bill_id, payment_id),
                CardResolution::LookUp,
            )
            .await
            .expect("looks up");
        assert!(matches!(resolved, CardOutcome::Captured { .. }));
        assert!(edge.parked_card(bill_id).is_none());
        assert_eq!(bill_state(&edge, bill_id), BillState::Settled);
        assert_eq!(terminal.authorisation_count(), 1, "the money moved once");

        // The replay agrees: settled, not parked, paid once.
        let restarted = edge_over(store.clone(), &terminal);
        restarted.rebuild().await.expect("rebuilds");
        assert!(restarted.parked_card(bill_id).is_none());
        assert_eq!(bill_state(&restarted, bill_id), BillState::Settled);
    });
}

#[test]
fn a_cancel_voids_the_attempt_and_the_bill_is_paid_another_way() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let terminal = FakePaymentTerminal::new();
        let edge = edge_over(store.clone(), &terminal);
        let (bill_id, owed) = an_open_bill(&edge, 5).await;
        terminal.stage_outcome(PaymentOutcome::Unknown);
        let CardOutcome::Unknown { payment_id } = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await
            .expect("asks")
        else {
            panic!("staged unknown");
        };

        let cancelled = edge
            .resolve_card_at(
                actor(),
                TillScope::default(),
                (bill_id, payment_id),
                CardResolution::Cancel,
            )
            .await
            .expect("cancels");
        assert!(matches!(cancelled, CardOutcome::Declined { .. }));
        assert!(edge.parked_card(bill_id).is_none());
        edge.settle_bill(actor(), bill_id, vec![cash(owed.amount_minor)], None)
            .await
            .expect("settles in cash");

        let outcomes: Vec<PaymentOutcome> = payments(&store)
            .await
            .iter()
            .map(|payment| payment.outcome.known())
            .collect();
        assert_eq!(
            outcomes,
            vec![
                PaymentOutcome::Unknown,
                PaymentOutcome::Declined,
                PaymentOutcome::Captured
            ]
        );
        // The replay counts the cash alone.
        let restarted = edge_over(store.clone(), &terminal);
        restarted.rebuild().await.expect("rebuilds");
        assert_eq!(bill_state(&restarted, bill_id), BillState::Settled);
        assert_eq!(
            restarted
                .lock_projection()
                .paid(bill_id)
                .map(|paid| (paid.cash, paid.other)),
            Some((owed, vnd(0)))
        );
    });
}

#[test]
fn a_cancel_of_a_card_the_acquirer_took_is_refused_and_a_confirm_records_it() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let terminal = FakePaymentTerminal::new();
        let edge = edge_over(store.clone(), &terminal);
        let (bill_id, owed) = an_open_bill(&edge, 6).await;
        terminal.stage_outcome(PaymentOutcome::Unknown);
        let CardOutcome::Unknown { payment_id } = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await
            .expect("asks")
        else {
            panic!("staged unknown");
        };
        terminal.conclude(payment_id, PaymentOutcome::Captured);

        let refused = edge
            .resolve_card_at(
                actor(),
                TillScope::default(),
                (bill_id, payment_id),
                CardResolution::Cancel,
            )
            .await;
        assert!(
            matches!(refused, Err(AppError::Card(CardRefusal::AlreadyCaptured))),
            "{refused:?}"
        );
        assert!(edge.parked_card(bill_id).is_some());

        // ADR-0243: by default a confirm by hand needs a manager, as a void does.
        let alone = edge
            .resolve_card_at(
                actor(),
                TillScope::default(),
                (bill_id, payment_id),
                CardResolution::Confirm,
            )
            .await;
        assert!(
            matches!(alone, Err(AppError::ApprovalRequired)),
            "{alone:?}"
        );
        assert!(edge.parked_card(bill_id).is_some(), "nothing changed");

        edge.apply_session(with_a_manager());
        let manager = crate::app::Approval {
            code: "MGR-1".to_owned(),
            pin: "4417".to_owned(),
        };
        let confirmed = edge
            .resolve_card_with(
                actor(),
                TillScope::default(),
                (bill_id, payment_id),
                CardResolution::Confirm,
                Some(&manager),
            )
            .await
            .expect("confirms with a manager");
        assert!(matches!(confirmed, CardOutcome::Captured { .. }));
        assert_eq!(bill_state(&edge, bill_id), BillState::Settled);
        let logged = store
            .read(&EventQuery::first(
                identity().store_id,
                NonZeroU32::new(200).expect("200 is not zero"),
            ))
            .await
            .expect("reads");
        let position = |wanted: EventType| {
            logged
                .iter()
                .rposition(|event| event.event_type.as_str() == wanted.as_str())
        };
        let approved = position(EventType::SecurityPermissionOverridden)
            .expect("the manager who agreed is recorded");
        let settled = position(EventType::BillingBillSettled).expect("the bill settled");
        assert!(
            approved < settled,
            "the manager is written in the capture's own transaction, before the settle that closes \
             it, not after it in one of its own"
        );
    });
}

#[test]
fn an_edge_with_no_terminal_says_so_and_a_card_is_recorded_by_hand() {
    pos_fakes::executor::run_ready(async {
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let (bill_id, owed) = an_open_bill(&edge, 7).await;
        assert!(!edge.takes_card_by_terminal());
        let refused = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await;
        assert!(
            matches!(refused, Err(AppError::Card(CardRefusal::TerminalNone))),
            "{refused:?}"
        );
        let resolved = edge
            .resolve_card_at(
                actor(),
                TillScope::default(),
                (bill_id, pos_proto::ids::PaymentId::new(Ulid::from_u128(9))),
                CardResolution::LookUp,
            )
            .await;
        assert!(
            matches!(resolved, Err(AppError::Card(CardRefusal::NotParked))),
            "{resolved:?}"
        );
    });
}

#[test]
fn a_card_for_more_than_the_bill_owes_never_reaches_the_terminal() {
    pos_fakes::executor::run_ready(async {
        let terminal = FakePaymentTerminal::new();
        let edge = edge_over(FakeStore::default(), &terminal);
        let (bill_id, owed) = an_open_bill(&edge, 8).await;
        let over = owed.checked_add(vnd(1)).expect("adds");
        let refused = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(over))
            .await;
        assert!(
            matches!(refused, Err(AppError::PaymentExceedsRemaining)),
            "{refused:?}"
        );
        assert_eq!(terminal.authorisation_count(), 0);
    });
}

/// ADR-0243 decision 1: a store that sets `CARD_CONFIRM_APPROVAL_NONE` confirms by hand alone.
#[test]
fn a_store_that_asks_no_manager_confirms_a_card_alone() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let terminal = FakePaymentTerminal::new();
        let edge = edge_over(store.clone(), &terminal);
        let mut session = EdgeSession::bootstrap();
        session.billing.card_confirm_approval =
            pos_proto::Open::from_known(pos_proto::billing::CardConfirmApproval::None);
        edge.apply_session(session);
        let (bill_id, owed) = an_open_bill(&edge, 7).await;
        terminal.stage_outcome(PaymentOutcome::Unknown);
        let CardOutcome::Unknown { payment_id } = edge
            .take_card_at(actor(), TillScope::default(), bill_id, ask(owed))
            .await
            .expect("asks")
        else {
            panic!("staged unknown");
        };
        let confirmed = edge
            .resolve_card_at(
                actor(),
                TillScope::default(),
                (bill_id, payment_id),
                CardResolution::Confirm,
            )
            .await
            .expect("confirms alone");
        assert!(matches!(confirmed, CardOutcome::Captured { .. }));
    });
}
