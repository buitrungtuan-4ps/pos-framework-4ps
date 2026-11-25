// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_contract_tests::fixtures;
use pos_fakes::FakeStore;
use pos_ports::event_store::EventStore as _;
use pos_ports::{PortError, Transactional as _, TxContext as _};
use pos_proto::einvoice::{EInvoiceKind, FiscalInvoice, InvoiceSymbol};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{EventType, FiscalInvoiceIssued};
use pos_proto::ids::{BillId, StoreId, TenantId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::time::CalendarDate;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use super::{
    InMemorySubmissions, STATE_PENDING, STATE_REJECTED, STATE_RETRYING, STATE_SUBMITTED,
    SubmissionStore, SubmitReport, feed_store, pass, submit_due,
};
use crate::dashboard::projector::StoreCatalog;
use crate::einvoice::sandbox::{SandboxEInvoice, SandboxMode};

fn store() -> StoreId {
    StoreId::new(Ulid::from_u128(0x0ADA))
}

fn tenant() -> TenantId {
    TenantId::new(Ulid::from_u128(0x7E11A))
}

/// `count` activations, then one issued invoice per `sequences`, all in `store()`'s log.
fn log(count: u32, sequences: &[u32]) -> Vec<EventEnvelope<RawPayload>> {
    let mut events = fixtures::activations(
        store(),
        1,
        count + u32::try_from(sequences.len()).unwrap_or(0),
    );
    let mut invoices = events.split_off(usize::try_from(count).unwrap_or(0));
    for (event, &sequence) in invoices.iter_mut().zip(sequences) {
        event.event_type = EventType::FiscalInvoiceIssued.into();
        event.data = RawPayload::encode(&FiscalInvoiceIssued {
            bill_id: BillId::new(Ulid::from_u128(u128::from(sequence))),
            invoice: FiscalInvoice {
                kind: Open::from_known(EInvoiceKind::Original),
                invoice_symbol: InvoiceSymbol::parse("1C26MAA").expect("a symbol"),
                invoice_sequence: sequence,
                issued_on: CalendarDate::from_ymd(2026, 11, 25).expect("a date"),
                total: Money::new(CurrencyCode::parse("VND").expect("VND"), 165_000),
                buyer_subject_id: None,
                connection_id: None,
            },
        })
        .expect("a payload");
    }
    events.extend(invoices);
    events
}

async fn append(events: &FakeStore, batch: &[EventEnvelope<RawPayload>]) {
    let mut tx = events.begin().await.expect("begin");
    events.append(&mut tx, batch).await.expect("append");
    tx.commit().await.expect("commit");
}

struct Catalog;

impl StoreCatalog for Catalog {
    async fn active_stores(&self) -> Result<Vec<(TenantId, StoreId)>, PortError> {
        Ok(vec![(tenant(), store())])
    }
}

#[tokio::test]
async fn each_issued_invoice_is_recorded_once_and_the_cursor_moves_past_the_rest() {
    let events = FakeStore::new();
    append(&events, &log(3, &[1, 2])).await;
    let submissions = InMemorySubmissions::default();
    assert_eq!(
        feed_store(&events, &submissions, tenant(), store(), 10).await,
        Ok(2)
    );
    assert_eq!(
        feed_store(&events, &submissions, tenant(), store(), 20).await,
        Ok(0)
    );
    let rows = submissions
        .list(tenant(), Some(store()), false)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row.state == STATE_PENDING && row.buyer_subject_id.is_none())
    );
    assert_eq!(rows[1].invoice_number, "1C26MAA/00000001");
    assert_eq!(
        (rows[1].total_minor, rows[1].currency.as_str()),
        (165_000, "VND")
    );
}

#[tokio::test]
async fn the_sandbox_takes_each_invoice_and_a_refusal_stops_while_a_timeout_retries() {
    let events = FakeStore::new();
    append(&events, &log(0, &[1])).await;
    let submissions = InMemorySubmissions::default();
    let provider = SandboxEInvoice::new();
    let report = pass(&events, &Catalog, &submissions, &provider, 1_000).await;
    assert_eq!(
        report,
        Ok(SubmitReport {
            submitted: 1,
            rejected: 0,
            retrying: 0
        })
    );
    let rows = submissions.list(tenant(), None, false).await.expect("list");
    assert_eq!(rows[0].state, STATE_SUBMITTED);
    assert!(rows[0].authority_reference.is_some());
    assert!(
        submissions
            .list(tenant(), None, true)
            .await
            .expect("list")
            .is_empty()
    );

    append(&events, &log(1, &[2])[1..]).await;
    provider.set_mode(SandboxMode::TimeOut);
    let report = pass(&events, &Catalog, &submissions, &provider, 2_000).await;
    assert_eq!(report.map(|report| report.retrying), Ok(1));
    let waiting = submissions.list(tenant(), None, true).await.expect("list");
    assert_eq!(waiting[0].state, STATE_RETRYING);
    assert_eq!(
        waiting[0].next_attempt_ms,
        2_000 + 60_000,
        "one minute after the first failure"
    );
    // Not due yet; then due, and refused, which stops it.
    assert_eq!(
        submit_due(&submissions, &provider, 3_000).await,
        Ok(SubmitReport::default())
    );
    provider.set_mode(SandboxMode::Refuse);
    let report = submit_due(&submissions, &provider, 62_000).await;
    assert_eq!(report.map(|report| report.rejected), Ok(1));
    let refused = submissions.list(tenant(), None, true).await.expect("list");
    assert_eq!(refused[0].state, STATE_REJECTED);
    assert!(refused[0].refusal.is_some());
    assert_eq!(
        submit_due(&submissions, &provider, 10_000_000).await,
        Ok(SubmitReport::default())
    );
}
