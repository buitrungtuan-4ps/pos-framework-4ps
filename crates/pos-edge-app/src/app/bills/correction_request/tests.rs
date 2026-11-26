// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A manager's correction request, and the till honouring HQ's approval (ADR-0250).

use std::num::NonZeroU32;

use pos_core::error::DomainError;
use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::BillState;
use pos_proto::einvoice::{
    CorrectionDecision, DecidedCorrection, EInvoiceIssueMoment, InvoicedCorrection,
    PublishedEInvoice,
};
use pos_proto::ids::BillId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::app::bills::void_settled::tests::{
    a_paid_bill, approval, card, over, reason, session, void,
};
use crate::app::tests::{actor, identity, vnd};
use crate::app::{AppError, Edge, EdgeSession};

/// A store that issues with each settle and corrects by `route`, a manager's limit of `limit`.
fn routed_with(route: InvoicedCorrection, limit: i64) -> EdgeSession {
    let mut session = session();
    session.einvoice = PublishedEInvoice {
        issue_moment: Open::from_known(EInvoiceIssueMoment::Immediate),
        symbol: Some("1C26MAA".to_owned()),
        invoiced_correction: Open::from_known(route),
        manager_correction_limit: Some(limit),
        ..PublishedEInvoice::default()
    };
    session
}

/// [`routed_with`] a manager's limit of 100k, below the 165k bill.
fn routed(route: InvoicedCorrection) -> EdgeSession {
    routed_with(route, 100_000)
}

/// HQ's `decision` on `bill_id`, published on the store's node as the cloud writes it.
fn decided(edge: &Edge<FakeStore>, bill_id: BillId, decision: CorrectionDecision) {
    let mut session = (*edge.session()).clone();
    session
        .einvoice
        .correction_decisions
        .push(DecidedCorrection {
            bill_id,
            decision: Open::from_known(decision),
            decided_by_user_id: Ulid::from_u128(0xAD),
            decided_time: Timestamp::from_milliseconds_since_epoch(1).expect("an instant"),
        });
    edge.apply_session(session);
}

async fn requested_events(store: &FakeStore) -> usize {
    store
        .read(&EventQuery::first(
            identity().store_id,
            NonZeroU32::new(500).expect("not zero"),
        ))
        .await
        .expect("reads")
        .iter()
        .filter(|event| event.event_type.as_str() == "fiscal.correction.requested")
        .count()
}

#[test]
fn a_request_is_recorded_and_the_void_waits_for_hqs_approval() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = over(store.clone(), routed(InvoicedCorrection::Request));
        let bill_id = a_paid_bill(&edge, 601, vec![card(165_000)]).await;
        let approval = approval("MGR-1");

        let asked = edge
            .request_invoice_correction(actor(), bill_id, Some(reason()), Some(&approval))
            .await
            .expect("a manager asks");
        assert_eq!(asked.amount, vnd(165_000));
        assert_eq!(requested_events(&store).await, 1);
        assert!(
            matches!(
                void(&edge, bill_id, Some("MGR-1")).await,
                Err(AppError::Domain(DomainError::EInvoiceIssued))
            ),
            "nothing leaves the drawer before HQ decides"
        );

        decided(&edge, bill_id, CorrectionDecision::Declined);
        assert!(matches!(
            void(&edge, bill_id, Some("MGR-1")).await,
            Err(AppError::Domain(DomainError::EInvoiceIssued))
        ));

        decided(&edge, bill_id, CorrectionDecision::Approved);
        let voided = void(&edge, bill_id, Some("MGR-1"))
            .await
            .expect("approved, the till voids it");
        assert_eq!(voided.state, BillState::Voided);
        assert!(
            void(&edge, bill_id, Some("MGR-1")).await.is_err(),
            "an approval is honoured once"
        );
    });
}

#[test]
fn only_a_route_that_asks_hq_takes_a_request() {
    pos_fakes::executor::run_ready(async {
        for (route, limit, refusal) in [
            (
                InvoicedCorrection::HqOnly,
                100_000,
                Some(DomainError::EInvoiceIssued),
            ),
            (InvoicedCorrection::ManagerUpToLimit, 100_000, None),
            (
                InvoicedCorrection::ManagerUpToLimit,
                200_000,
                Some(DomainError::CorrectionNotNeeded),
            ),
        ] {
            let edge = over(FakeStore::default(), routed_with(route, limit));
            let bill_id = a_paid_bill(&edge, 602, vec![card(165_000)]).await;
            let asked = edge
                .request_invoice_correction(
                    actor(),
                    bill_id,
                    Some(reason()),
                    Some(&approval("MGR-1")),
                )
                .await;
            match refusal {
                None => assert!(asked.is_ok(), "{route:?} {limit}: {asked:?}"),
                Some(expected) => assert!(
                    matches!(&asked, Err(AppError::Domain(got)) if *got == expected),
                    "{route:?} {limit}: {asked:?}"
                ),
            }
        }
    });
}

#[test]
fn a_reason_is_required_unless_the_store_turns_it_off() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = over(store.clone(), routed(InvoicedCorrection::Request));
        let bill_id = a_paid_bill(&edge, 603, vec![card(165_000)]).await;
        let manager = approval("MGR-1");
        assert!(matches!(
            edge.request_invoice_correction(actor(), bill_id, None, Some(&manager))
                .await,
            Err(AppError::VoidReasonNotValid)
        ));
        assert_eq!(requested_events(&store).await, 0);

        let mut optional = routed(InvoicedCorrection::Request);
        optional.einvoice.correction_reason_required = Some(false);
        edge.apply_session(optional);
        edge.request_invoice_correction(actor(), bill_id, None, Some(&manager))
            .await
            .expect("no reason needed");
        assert_eq!(requested_events(&store).await, 1);
    });
}

/// HQ's `decision` on `bill_id` at `decided_ms`, published on the store's node.
fn decided_at(
    edge: &Edge<FakeStore>,
    bill_id: BillId,
    decision: CorrectionDecision,
    decided_ms: i64,
) {
    let mut session = (*edge.session()).clone();
    session
        .einvoice
        .correction_decisions
        .push(DecidedCorrection {
            bill_id,
            decision: Open::from_known(decision),
            decided_by_user_id: Ulid::from_u128(0xAD),
            decided_time: Timestamp::from_milliseconds_since_epoch(decided_ms).expect("an instant"),
        });
    edge.apply_session(session);
}

async fn decided_events(store: &FakeStore) -> Vec<DecidedCorrection> {
    store
        .read(&EventQuery::first(
            identity().store_id,
            NonZeroU32::new(500).expect("not zero"),
        ))
        .await
        .expect("reads")
        .iter()
        .filter(|event| event.event_type.as_str() == "fiscal.correction.decided")
        .map(|event| {
            event
                .data
                .decode::<pos_proto::events::FiscalCorrectionDecided>()
                .expect("decodes")
                .decision
        })
        .collect()
}

#[test]
fn the_store_records_each_decision_on_a_bill_it_asked_about_once() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = over(store.clone(), routed(InvoicedCorrection::Request));
        let asked = a_paid_bill(&edge, 604, vec![card(165_000)]).await;
        let other = a_paid_bill(&edge, 605, vec![card(165_000)]).await;
        edge.request_invoice_correction(actor(), asked, Some(reason()), Some(&approval("MGR-1")))
            .await
            .expect("a manager asks");

        decided_at(&edge, asked, CorrectionDecision::Declined, 10);
        decided_at(&edge, other, CorrectionDecision::Approved, 10);
        assert_eq!(
            edge.record_correction_decisions().await.ok(),
            Some(1),
            "only the bill it asked about"
        );
        assert_eq!(
            edge.record_correction_decisions().await.ok(),
            Some(0),
            "recorded once"
        );

        // A restart folds the log and records nothing again.
        let restarted = over(store.clone(), (*edge.session()).clone());
        restarted.rebuild().await.expect("rebuilds");
        assert_eq!(restarted.record_correction_decisions().await.ok(), Some(0));

        // HQ changes its mind later: the newer decision is recorded too.
        decided_at(&restarted, asked, CorrectionDecision::Approved, 20);
        assert_eq!(restarted.record_correction_decisions().await.ok(), Some(1));
        let recorded = decided_events(&store).await;
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].bill_id, asked);
        assert!(!recorded[0].approved());
        assert!(recorded[1].approved());
        let envelope_employee = store
            .read(&EventQuery::first(
                identity().store_id,
                NonZeroU32::new(500).expect("not zero"),
            ))
            .await
            .expect("reads")
            .into_iter()
            .rfind(|event| event.event_type.as_str() == "fiscal.correction.decided")
            .and_then(|event| event.employee_id);
        assert_ne!(
            envelope_employee,
            Some(actor().employee_id),
            "stamped by the box, not a person"
        );
    });
}
