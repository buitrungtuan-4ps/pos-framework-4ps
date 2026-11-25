// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A settle issues its e-invoice in the sale's transaction (ADR-0225 decisions 2, 3 and 5).

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::EventPayload;
use pos_proto::einvoice::{EInvoiceIssueMoment, EInvoiceKind, PublishedEInvoice};
use pos_proto::events::{FiscalInvoiceIssued, FiscalInvoiceScheduled};
use pos_proto::ids::{BillId, TableId};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::app::tests::{a_line, actor, cash, identity, vnd};
use crate::app::{AppError, BuyerDetails, Edge, EdgeSession};
use crate::receipt::InMemoryReceipts;

fn a_store(store: FakeStore, moment: EInvoiceIssueMoment, symbol: Option<&str>) -> Edge<FakeStore> {
    a_scheduled_store(store, moment, symbol, (0, 1_320))
}

/// A store under `moment` with `(delay_minutes, batch_minute)`.
fn a_scheduled_store(
    store: FakeStore,
    moment: EInvoiceIssueMoment,
    symbol: Option<&str>,
    (delay_minutes, batch_minute): (i64, i64),
) -> Edge<FakeStore> {
    let session = EdgeSession {
        einvoice: PublishedEInvoice {
            issue_moment: Open::from_known(moment),
            symbol: symbol.map(str::to_owned),
            delay_minutes: Some(delay_minutes),
            batch_minute: Some(batch_minute),
            ..PublishedEInvoice::default()
        },
        ..EdgeSession::bootstrap()
    };
    Edge::new(
        store,
        identity(),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds")
}

/// Seats `table`, sells one 150,000 line and settles its bill for the 165,000 it comes to.
async fn settle_one(
    edge: &Edge<FakeStore>,
    table: u128,
    buyer: Option<&BuyerDetails>,
) -> Result<(BillId, Option<u64>), AppError> {
    let table = TableId::new(Ulid::from_u128(table));
    edge.seat_table(actor(), table, None).await.expect("seats");
    edge.add_line(actor(), table, a_line()).await.expect("adds");
    let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
    let settled = edge
        .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], buyer)
        .await?;
    Ok((opened.bill_id, settled.receipt_number))
}

async fn invoices(store: &FakeStore) -> Vec<FiscalInvoiceIssued> {
    store
        .read(&EventQuery::first(
            identity().store_id,
            NonZeroU32::new(200).expect("200 is not zero"),
        ))
        .await
        .expect("reads the log")
        .iter()
        .filter(|envelope| envelope.event_type.as_str() == FiscalInvoiceIssued::EVENT_TYPE.as_str())
        .map(|envelope| envelope.data.decode().expect("decodes"))
        .collect()
}

fn buyer() -> BuyerDetails {
    BuyerDetails {
        name: "Cong ty Mau".to_owned(),
        tax_code: Some("0101243150".to_owned()),
        address: None,
        email: None,
    }
}

#[test]
fn a_store_that_issues_none_writes_no_invoice() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(store.clone(), EInvoiceIssueMoment::Off, Some("1C26MAA"));
        settle_one(&edge, 1, Some(&buyer())).await.expect("settles");
        assert!(invoices(&store).await.is_empty());
    });
}

#[test]
fn every_sale_numbers_its_invoice_gaplessly_under_the_stores_symbol() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            Some("1C26MAA"),
        );
        let (first, _) = settle_one(&edge, 1, None).await.expect("settles");
        let (second, _) = settle_one(&edge, 2, Some(&buyer())).await.expect("settles");

        let issued = invoices(&store).await;
        let numbers: Vec<(BillId, &str, u32)> = issued
            .iter()
            .map(|event| {
                (
                    event.bill_id,
                    event.invoice.invoice_symbol.as_str(),
                    event.invoice.invoice_sequence,
                )
            })
            .collect();
        assert_eq!(numbers, [(first, "1C26MAA", 1), (second, "1C26MAA", 2)]);
        let plain = issued.first().expect("the first invoice");
        let named = issued.get(1).expect("the second invoice");
        assert_eq!(plain.invoice.kind, Open::from_known(EInvoiceKind::Original));
        assert_eq!(plain.invoice.total, vnd(165_000));
        assert_eq!(plain.invoice.buyer_subject_id, None);
        assert!(
            named.invoice.buyer_subject_id.is_some(),
            "a buyer is named by id only; the details are in the subject store"
        );
        let payload = serde_json::to_string(named).expect("serializes");
        for personal in ["Cong ty Mau", "0101243150"] {
            assert!(!payload.contains(personal), "{personal} reached the event");
        }
    });
}

#[test]
fn a_restart_continues_the_sequence_where_it_stopped() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        {
            let edge = a_store(
                store.clone(),
                EInvoiceIssueMoment::Immediate,
                Some("1C26MAA"),
            );
            settle_one(&edge, 1, None).await.expect("settles");
        }
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            Some("1C26MAA"),
        );
        edge.rebuild().await.expect("rebuilds from the log");
        settle_one(&edge, 2, None).await.expect("settles");
        let sequences: Vec<u32> = invoices(&store)
            .await
            .iter()
            .map(|event| event.invoice.invoice_sequence)
            .collect();
        assert_eq!(sequences, [1, 2], "never a repeated number after a restart");
    });
}

#[test]
fn a_store_that_must_issue_with_no_symbol_refuses_the_sale_and_takes_no_number() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(store.clone(), EInvoiceIssueMoment::Immediate, None);
        let refused = settle_one(&edge, 1, None).await;
        assert!(
            matches!(refused, Err(AppError::EInvoiceSymbolMissing)),
            "{refused:?}"
        );
        assert!(invoices(&store).await.is_empty());

        edge.apply_session(EdgeSession {
            einvoice: PublishedEInvoice {
                issue_moment: Open::from_known(EInvoiceIssueMoment::Immediate),
                symbol: Some("1C26MAA".to_owned()),
                ..PublishedEInvoice::default()
            },
            ..EdgeSession::bootstrap()
        });
        let table = TableId::new(Ulid::from_u128(1));
        let bill = edge.open_bill(actor(), table).await;
        let bill_id = match bill {
            Ok(opened) => opened.bill_id,
            Err(_already_open) => edge
                .lock_projection()
                .bills
                .keys()
                .next()
                .copied()
                .expect("the refused settle left its bill open"),
        };
        let settled = edge
            .settle_bill(actor(), bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles once the symbol arrives");
        assert_eq!(
            settled.receipt_number,
            Some(1),
            "the refused settle consumed no receipt number"
        );
    });
}

#[test]
fn on_request_issues_only_for_a_bill_with_a_buyer() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::OnRequest,
            Some("1C26MAB"),
        );
        settle_one(&edge, 1, None).await.expect("settles");
        let (named, _) = settle_one(&edge, 2, Some(&buyer())).await.expect("settles");
        let issued = invoices(&store).await;
        assert_eq!(issued.len(), 1);
        assert_eq!(issued.first().map(|event| event.bill_id), Some(named));
        assert!(edge.lock_projection().invoices.issued_for(named));
    });
}

async fn scheduled(store: &FakeStore) -> Vec<FiscalInvoiceScheduled> {
    store
        .read(&EventQuery::first(
            identity().store_id,
            NonZeroU32::new(200).expect("200 is not zero"),
        ))
        .await
        .expect("reads the log")
        .iter()
        .filter(|envelope| {
            envelope.event_type.as_str() == FiscalInvoiceScheduled::EVENT_TYPE.as_str()
        })
        .map(|envelope| envelope.data.decode().expect("decodes"))
        .collect()
}

/// ADR-0229 decisions 3 and 4: a deferred invoice is recorded at settle with no number, and takes
/// the next number when it is issued, in due order.
#[test]
fn a_scheduled_invoice_takes_its_number_when_it_is_issued() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_scheduled_store(
            store.clone(),
            EInvoiceIssueMoment::ScheduledPerOrder,
            Some("1C26MAA"),
            (0, 1_320),
        );
        let (first, _) = settle_one(&edge, 1, None).await.expect("settles");
        let (second, _) = settle_one(&edge, 2, Some(&buyer())).await.expect("settles");
        assert!(
            invoices(&store).await.is_empty(),
            "nothing is numbered at settle"
        );
        let waiting = scheduled(&store).await;
        assert_eq!(waiting.len(), 2);
        assert!(
            waiting
                .get(1)
                .and_then(|record| record.schedule.buyer_subject_id)
                .is_some(),
            "the buyer is carried by id only"
        );

        assert_eq!(edge.issue_due_invoices().await.expect("issues"), 2);
        let numbers: Vec<(BillId, u32)> = invoices(&store)
            .await
            .iter()
            .map(|event| (event.bill_id, event.invoice.invoice_sequence))
            .collect();
        assert_eq!(numbers, [(first, 1), (second, 2)]);
        assert_eq!(
            edge.issue_due_invoices().await.expect("issues"),
            0,
            "an issued invoice is not issued again"
        );
    });
}

/// A batch window not yet reached issues nothing, and the waiting invoice survives a restart.
#[test]
fn a_batch_waits_for_its_window_and_survives_a_restart() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let batch = (0, 1_320);
        {
            let edge = a_scheduled_store(
                store.clone(),
                EInvoiceIssueMoment::BatchWindow,
                Some("1C26MAA"),
                batch,
            );
            settle_one(&edge, 1, None).await.expect("settles");
            assert_eq!(edge.issue_due_invoices().await.expect("ticks"), 0);
        }
        let edge = a_scheduled_store(
            store.clone(),
            EInvoiceIssueMoment::BatchWindow,
            Some("1C26MAA"),
            batch,
        );
        edge.rebuild().await.expect("rebuilds from the log");
        let due = scheduled(&store)
            .await
            .first()
            .map(|record| record.schedule.due_time)
            .expect("a schedule");
        let waiting = edge.lock_projection().invoices.due_at(due, 10).len();
        assert_eq!(waiting, 1, "the restart still knows the invoice is waiting");
        assert!(invoices(&store).await.is_empty());
    });
}

/// ADR-0229 decision 3: a bill voided while its invoice waits leaves the queue and takes no number,
/// so the next invoice continues the sequence with no gap.
#[test]
fn a_bill_voided_while_waiting_takes_no_number() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_scheduled_store(
            store.clone(),
            EInvoiceIssueMoment::ScheduledPerOrder,
            Some("1C26MAA"),
            (0, 1_320),
        );
        let (voided, _) = settle_one(&edge, 1, None).await.expect("settles");
        let (kept, _) = settle_one(&edge, 2, None).await.expect("settles");
        edge.lock_projection().invoices.drop_pending(voided);
        assert_eq!(edge.issue_due_invoices().await.expect("issues"), 1);
        let numbers: Vec<(BillId, u32)> = invoices(&store)
            .await
            .iter()
            .map(|event| (event.bill_id, event.invoice.invoice_sequence))
            .collect();
        assert_eq!(numbers, [(kept, 1)]);
    });
}

/// ADR-0229 decision 2: the country's limit clamps a schedule that would issue too late.
#[test]
fn the_countrys_limit_clamps_a_late_schedule() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_scheduled_store(
            store.clone(),
            EInvoiceIssueMoment::ScheduledPerOrder,
            Some("1C26MAA"),
            (1_440, 1_320),
        );
        edge.limit_invoice_issue(Some(pos_proto::einvoice::IssueLimit::MinutesAfterSale(0)));
        settle_one(&edge, 1, None).await.expect("settles");
        assert_eq!(
            edge.issue_due_invoices().await.expect("issues"),
            1,
            "a day's delay is held to the limit, here at once"
        );
    });
}

/// ADR-0242 decisions 1 and 2: a settle that issues its invoice answers with the number and the
/// last ten characters of the invoice event's id; a deferred invoice answers neither.
#[test]
fn a_settle_answers_with_the_invoice_number_and_its_lookup_code() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            Some("1C26MAA"),
        );
        let table = TableId::new(Ulid::from_u128(7));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens");
        let settled = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles");
        let invoice = settled
            .invoice
            .expect("an invoice was issued with the settle");
        assert_eq!(invoice.number, "1C26MAA/00000001");
        let event = store
            .read(&EventQuery::first(
                identity().store_id,
                NonZeroU32::new(200).expect("200 is not zero"),
            ))
            .await
            .expect("reads")
            .into_iter()
            .find(|envelope| {
                envelope.event_type.as_str() == FiscalInvoiceIssued::EVENT_TYPE.as_str()
            })
            .expect("the invoice event");
        let id = event.event_id.to_string();
        assert_eq!(invoice.lookup_code.len(), 10);
        assert!(id.ends_with(&invoice.lookup_code));

        let later = a_store(
            FakeStore::default(),
            EInvoiceIssueMoment::BatchWindow,
            Some("1C26MAA"),
        );
        later.seat_table(actor(), table, None).await.expect("seats");
        later
            .add_line(actor(), table, a_line())
            .await
            .expect("adds");
        let opened = later.open_bill(actor(), table).await.expect("opens");
        let settled = later
            .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles");
        assert_eq!(
            settled.invoice, None,
            "a deferred invoice has no number yet"
        );
    });
}

/// ADR-0242 decision 3: a copy of the receipt carries the invoice the receipt printed, the same
/// number and lookup code, and still does after a restart rebuilds the store from its log.
#[test]
fn a_receipt_copy_carries_the_invoice_also_after_a_restart() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            Some("1C26MAA"),
        );
        let table = TableId::new(Ulid::from_u128(7));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens");
        let settled = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles");
        let printed = settled
            .invoice
            .expect("an invoice was issued with the settle");

        let copy = edge
            .reprint_receipt(actor(), opened.bill_id)
            .await
            .expect("copies");
        assert_eq!(copy.invoice.as_ref(), Some(&printed));

        edge.rebuild().await.expect("rebuilds from the log");
        let again = edge
            .reprint_receipt(actor(), opened.bill_id)
            .await
            .expect("copies after the restart");
        assert_eq!(again.invoice, Some(printed));
    });
}

/// A copy of a bill whose invoice is still waiting prints no number: it has none yet.
#[test]
fn a_receipt_copy_of_a_deferred_invoice_has_no_number_yet() {
    pos_fakes::executor::run_ready(async {
        let edge = a_store(
            FakeStore::default(),
            EInvoiceIssueMoment::BatchWindow,
            Some("1C26MAA"),
        );
        let table = TableId::new(Ulid::from_u128(7));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens");
        edge.settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles");
        let copy = edge
            .reprint_receipt(actor(), opened.bill_id)
            .await
            .expect("copies");
        assert_eq!(copy.invoice, None);
    });
}
