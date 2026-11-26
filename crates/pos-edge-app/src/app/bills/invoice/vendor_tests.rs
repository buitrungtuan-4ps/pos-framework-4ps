// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Under `einvoice.number_source` `VENDOR` a settle issues its e-invoice "number pending" and the
//! store's own sequence never moves
//! ([ADR-0251](../../../../../../docs/adr/0251-a-vendor-numbered-invoice-waits-for-its-number-and-the-store-learns-it.md)).

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::EventPayload;
use pos_proto::einvoice::{
    EInvoiceIssueMoment, EInvoiceNumberSource, PublishedEInvoice, VendorCode, VendorNumber,
};
use pos_proto::events::{FiscalInvoiceIssued, FiscalInvoiceNumbered};
use pos_proto::ids::{BillId, TableId};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::app::tests::{a_line, actor, cash, identity};
use crate::app::{Edge, EdgeSession, IssuedInvoiceRef};
use crate::receipt::InMemoryReceipts;

fn a_store(
    store: FakeStore,
    moment: EInvoiceIssueMoment,
    source: EInvoiceNumberSource,
) -> Edge<FakeStore> {
    let session = EdgeSession {
        einvoice: PublishedEInvoice {
            issue_moment: Open::from_known(moment),
            symbol: Some("1C26MAA".to_owned()),
            delay_minutes: Some(0),
            number_source: Open::from_known(source),
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

/// Settles one 165,000 bill on `table`; answers its bill and the invoice the settle answered with.
async fn settle_one(edge: &Edge<FakeStore>, table: u128) -> (BillId, Option<IssuedInvoiceRef>) {
    let table = TableId::new(Ulid::from_u128(table));
    edge.seat_table(actor(), table, None).await.expect("seats");
    edge.add_line(actor(), table, a_line()).await.expect("adds");
    let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
    let settled = edge
        .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
        .await
        .expect("settles");
    (opened.bill_id, settled.invoice)
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

#[test]
fn a_vendor_numbered_invoice_is_issued_with_the_sale_and_no_number() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            EInvoiceNumberSource::Vendor,
        );
        let (bill_id, answered) = settle_one(&edge, 1).await;

        let answered = answered.expect("the settle still issues its invoice");
        assert_eq!(answered.number, None, "the vendor gives the number later");
        assert_eq!(
            answered.lookup_code.len(),
            10,
            "the lookup code works offline"
        );

        let issued = invoices(&store).await;
        assert_eq!(issued.len(), 1);
        let invoice = &issued[0].invoice;
        assert_eq!(issued[0].bill_id, bill_id);
        assert_eq!(invoice.invoice_sequence, 0);
        assert_eq!(invoice.invoice_symbol.as_str(), "1C26MAA");
        assert_eq!(invoice.number_source.known(), EInvoiceNumberSource::Vendor);
        assert!(invoice.number_pending());

        let copy = edge
            .reprint_receipt(actor(), bill_id)
            .await
            .expect("copies");
        assert_eq!(copy.invoice, Some(answered));
    });
}

#[test]
fn a_store_switched_back_to_the_pos_waits_until_no_number_is_pending() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        {
            let edge = a_store(
                store.clone(),
                EInvoiceIssueMoment::Immediate,
                EInvoiceNumberSource::Vendor,
            );
            settle_one(&edge, 1).await;
        }
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            EInvoiceNumberSource::Pos,
        );
        edge.rebuild().await.expect("rebuilds from the log");
        let (_, answered) = settle_one(&edge, 2).await;
        assert_eq!(
            answered.expect("an invoice").number,
            None,
            "never numbers over a number the vendor is about to give"
        );
        let sequences: Vec<(u32, bool)> = invoices(&store)
            .await
            .iter()
            .map(|event| {
                (
                    event.invoice.invoice_sequence,
                    event.invoice.number_pending(),
                )
            })
            .collect();
        assert_eq!(sequences, [(0, true), (0, true)]);
    });
}

#[test]
fn a_pos_numbered_store_is_unchanged() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            EInvoiceNumberSource::Pos,
        );
        let (_, answered) = settle_one(&edge, 1).await;
        assert_eq!(
            answered.expect("an invoice").number.as_deref(),
            Some("1C26MAA/00000001")
        );
        let issued = invoices(&store).await;
        assert_eq!(
            issued[0].invoice.number_source.known(),
            EInvoiceNumberSource::Pos
        );
        assert!(!issued[0].invoice.number_pending());
    });
}

#[test]
fn a_deferred_vendor_invoice_is_issued_number_pending_when_it_falls_due() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::ScheduledPerOrder,
            EInvoiceNumberSource::Hybrid,
        );
        let (bill_id, answered) = settle_one(&edge, 1).await;
        assert_eq!(
            answered, None,
            "a deferred invoice is not issued with the settle"
        );
        assert_eq!(edge.issue_due_invoices().await.expect("issues"), 1);
        let issued = invoices(&store).await;
        assert_eq!(issued[0].bill_id, bill_id);
        assert!(
            issued[0].invoice.number_pending(),
            "HYBRID without an offline symbol waits for the vendor"
        );
    });
}

/// The vendor's `number` for `bill_id`, published on the store's node as the cloud writes it, under
/// `source`.
fn numbered(
    edge: &Edge<FakeStore>,
    source: EInvoiceNumberSource,
    (bill_id, number): (BillId, &str),
) {
    let mut session = (*edge.session()).clone();
    session.einvoice.number_source = Open::from_known(source);
    session.einvoice.vendor_numbers.push(VendorNumber {
        bill_id,
        invoice_number: VendorCode::new(number),
        authority_reference: Some(VendorCode::new("SANDBOX-REF")),
    });
    edge.apply_session(session);
}

async fn numbered_events(store: &FakeStore) -> Vec<FiscalInvoiceNumbered> {
    store
        .read(&EventQuery::first(
            identity().store_id,
            NonZeroU32::new(200).expect("200 is not zero"),
        ))
        .await
        .expect("reads the log")
        .iter()
        .filter(|envelope| {
            envelope.event_type.as_str() == FiscalInvoiceNumbered::EVENT_TYPE.as_str()
        })
        .map(|envelope| envelope.data.decode().expect("decodes"))
        .collect()
}

#[test]
fn the_store_records_the_vendors_number_once_and_a_copy_prints_it() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            EInvoiceNumberSource::Vendor,
        );
        let (first, _) = settle_one(&edge, 1).await;
        let (second, _) = settle_one(&edge, 2).await;
        assert_eq!(
            edge.record_vendor_numbers().await.ok(),
            Some(0),
            "nothing published yet"
        );

        numbered(
            &edge,
            EInvoiceNumberSource::Vendor,
            (first, "1C26MAA/00000005"),
        );
        assert_eq!(edge.record_vendor_numbers().await.ok(), Some(1));
        assert_eq!(
            edge.record_vendor_numbers().await.ok(),
            Some(0),
            "recorded once"
        );
        let recorded = numbered_events(&store).await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].number.bill_id, first);

        let copy = edge.reprint_receipt(actor(), first).await.expect("copies");
        assert_eq!(
            copy.invoice.and_then(|invoice| invoice.number).as_deref(),
            Some("1C26MAA/00000005")
        );
        let waiting = edge.reprint_receipt(actor(), second).await.expect("copies");
        assert_eq!(
            waiting.invoice.map(|invoice| invoice.number),
            Some(None),
            "still pending"
        );

        edge.rebuild().await.expect("rebuilds from the log");
        assert_eq!(
            edge.record_vendor_numbers().await.ok(),
            Some(0),
            "none twice after a restart"
        );
        let copy = edge.reprint_receipt(actor(), first).await.expect("copies");
        assert_eq!(
            copy.invoice.and_then(|invoice| invoice.number).as_deref(),
            Some("1C26MAA/00000005")
        );
    });
}

#[test]
fn a_store_switched_back_numbers_after_the_vendors_last_number_once_none_waits() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = a_store(
            store.clone(),
            EInvoiceIssueMoment::Immediate,
            EInvoiceNumberSource::Vendor,
        );
        let (first, _) = settle_one(&edge, 1).await;
        numbered(
            &edge,
            EInvoiceNumberSource::Pos,
            (first, "1C26MAA/00000006"),
        );
        assert_eq!(edge.record_vendor_numbers().await.ok(), Some(1));
        let (_, answered) = settle_one(&edge, 2).await;
        assert_eq!(
            answered.and_then(|invoice| invoice.number).as_deref(),
            Some("1C26MAA/00000007"),
            "the store numbers after the vendor's last number, never over it"
        );
    });
}
