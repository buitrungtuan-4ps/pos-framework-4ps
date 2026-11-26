// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_contract_tests::fixtures;
use pos_fakes::FakeStore;
use pos_ports::event_store::EventStore as _;
use pos_ports::{PortError, Transactional as _, TxContext as _};
use std::sync::Arc;

use pos_proto::einvoice::{
    EInvoiceKind, EInvoiceNumberSource, FiscalInvoice, InvoiceSymbol, PublishedEInvoice,
};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{BillingBillVoided, EventType, FiscalInvoiceIssued};
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
use crate::einvoice::corrections::{Corrector, StoreEInvoice};
use crate::einvoice::feed::FeedFuture;
use crate::einvoice::requests::{CorrectionRequests as _, InMemoryCorrectionRequests};
use crate::einvoice::sandbox::{SandboxEInvoice, SandboxMode};
use crate::einvoice::symbols::{
    InMemorySymbolLedger, SymbolLedger as _, SymbolRecord, SymbolSource,
};
use crate::einvoice::vendor_numbers::{Recorded, is_pending};

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
                number_source: Open::default(),
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

/// A store whose `einvoice` node says `correction_kind`, in no country.
struct Node(&'static str);

impl StoreEInvoice for Node {
    fn read(
        &self,
        _tenant: TenantId,
        _store: StoreId,
    ) -> FeedFuture<'_, (PublishedEInvoice, Option<pos_proto::locale::CountryCode>)> {
        let node = serde_json::from_value(serde_json::json!({ "correction_kind": self.0 }))
            .unwrap_or_default();
        Box::pin(async move { Ok((node, None)) })
    }
}

/// A corrector under `kind`, whose tenant holds the correction symbol `1K26TAA`.
async fn corrector(kind: &'static str) -> Corrector {
    let ledger = InMemorySymbolLedger::default();
    ledger
        .record(
            tenant(),
            SymbolRecord {
                symbol: "1K26TAA".to_owned(),
                store_id: None,
                lease_generation: None,
                source: SymbolSource::Manual,
                recorded_at_ms: 0,
            },
        )
        .await
        .expect("a symbol");
    Corrector {
        settings: Arc::new(Node(kind)),
        ledger: Arc::new(ledger),
        requests: Arc::new(InMemoryCorrectionRequests::default()),
        numbers: Arc::new(Recorded::default()),
    }
}

/// `billing.bill.voided` of the bill whose invoice is `sequence` in [`log`], as event `id`.
fn voided(sequence: u32, id: u128) -> EventEnvelope<RawPayload> {
    let mut event = fixtures::activations(store(), 1, 1).remove(0);
    event.event_id = pos_proto::ids::EventId::new(Ulid::from_u128(id));
    event.event_type = EventType::BillingBillVoided.into();
    event.data = RawPayload::encode(&BillingBillVoided {
        bill_id: BillId::new(Ulid::from_u128(u128::from(sequence))),
        reason_code_id: pos_proto::ids::ReasonCodeId::new(Ulid::from_u128(0x5EA)),
    })
    .expect("a payload");
    event
}

/// `fiscal.correction.requested` for the bill whose invoice is `sequence`, by employee `0xE1`.
fn requested(sequence: u32, id: u128) -> EventEnvelope<RawPayload> {
    let mut event = fixtures::activations(store(), 1, 1).remove(0);
    event.event_id = pos_proto::ids::EventId::new(Ulid::from_u128(id));
    event.employee_id = Some(pos_proto::ids::EmployeeId::new(Ulid::from_u128(0xE1)));
    event.event_type = EventType::FiscalCorrectionRequested.into();
    event.data = RawPayload::encode(&pos_proto::events::FiscalCorrectionRequested {
        request: pos_proto::einvoice::CorrectionRequest {
            bill_id: BillId::new(Ulid::from_u128(u128::from(sequence))),
            amount: Money::new(CurrencyCode::parse("VND").expect("VND"), 165_000),
            kind: Open::from_known(EInvoiceKind::Replacement),
            reason_code_id: Some(pos_proto::ids::ReasonCodeId::new(Ulid::from_u128(0x5EA))),
        },
    })
    .expect("a payload");
    event
}

#[tokio::test]
async fn a_correction_request_is_queued_once_for_hq_and_issues_nothing() {
    let events = FakeStore::new();
    let mut batch = log(0, &[1]);
    batch.push(requested(1, u128::MAX));
    append(&events, &batch).await;
    let submissions = InMemorySubmissions::default();
    let queue = InMemoryCorrectionRequests::default();
    let mut corrector = corrector("EINVOICE_KIND_ADJUSTMENT").await;
    corrector.requests = Arc::new(queue.clone());
    assert_eq!(
        feed_store(&events, &submissions, &corrector, tenant(), store(), NOW_MS).await,
        Ok(1),
        "the original alone: a request issues nothing"
    );
    let queued = queue.list(tenant(), None).await.expect("list");
    assert_eq!(queued.len(), 1);
    let row = &queued[0];
    assert_eq!(
        (
            row.status.as_str(),
            row.bill_id.as_str(),
            row.amount_minor,
            row.kind.as_str(),
            row.requested_by.as_deref(),
        ),
        (
            "PENDING",
            Ulid::from_u128(1).to_string().as_str(),
            165_000,
            "REPLACEMENT",
            Some(Ulid::from_u128(0xE1).to_string().as_str()),
        )
    );
    assert_eq!(row.request_id, Ulid::from_u128(u128::MAX).to_string());
    assert_eq!(
        queue.record(row.clone()).await,
        Ok(false),
        "a request read twice is queued once"
    );
}

/// 2026-11-25, so the correction symbol of 2026 is this year's.
const NOW_MS: i64 = 1_795_564_800_000;

#[tokio::test]
async fn a_voided_invoiced_bill_is_corrected_once_under_the_stores_kind() {
    for (kind, total) in [
        ("EINVOICE_KIND_ADJUSTMENT", -165_000),
        ("EINVOICE_KIND_REPLACEMENT", 0),
    ] {
        let events = FakeStore::new();
        let mut batch = log(0, &[1, 2]);
        // Bill 1 is voided; a bill with no invoice is voided too, and needs no correction.
        batch.push(voided(1, u128::MAX - 1));
        batch.push(voided(9, u128::MAX));
        append(&events, &batch).await;
        let submissions = InMemorySubmissions::default();
        let corrector = corrector(kind).await;
        assert_eq!(
            feed_store(&events, &submissions, &corrector, tenant(), store(), NOW_MS).await,
            Ok(3),
            "{kind}: two originals and one correction"
        );
        let rows = submissions.list(tenant(), None, false).await.expect("list");
        let correction: Vec<_> = rows
            .iter()
            .filter(|row| row.corrects_invoice_number.is_some())
            .collect();
        assert_eq!(correction.len(), 1, "{kind}");
        let row = correction[0];
        assert_eq!(row.invoice_number, "1K26TAA/00000001", "{kind}");
        assert_eq!(
            row.corrects_invoice_number.as_deref(),
            Some("1C26MAA/00000001"),
            "{kind}"
        );
        assert_eq!(
            row.kind,
            kind.trim_start_matches("EINVOICE_KIND_"),
            "{kind}"
        );
        assert_eq!(row.total_minor, total, "{kind}");
        assert_eq!(
            (row.state.as_str(), row.issued_on.as_str()),
            (STATE_PENDING, "2026-11-25")
        );

        // Read again from the start: nothing new, no second number.
        let again = InMemorySubmissions::default();
        for row in rows {
            again.record(row).await.expect("copy");
        }
        assert_eq!(
            feed_store(&events, &again, &corrector, tenant(), store(), NOW_MS + 1).await,
            Ok(0),
            "{kind}: a void read twice is corrected once"
        );
    }
}

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
        feed_store(
            &events,
            &submissions,
            &corrector("").await,
            tenant(),
            store(),
            10
        )
        .await,
        Ok(2)
    );
    assert_eq!(
        feed_store(
            &events,
            &submissions,
            &corrector("").await,
            tenant(),
            store(),
            20
        )
        .await,
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
    let report = pass(
        &events,
        &Catalog,
        &submissions,
        &corrector("").await,
        &provider,
        1_000,
    )
    .await;
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
    let report = pass(
        &events,
        &Catalog,
        &submissions,
        &corrector("").await,
        &provider,
        2_000,
    )
    .await;
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
        submit_due(&submissions, &Recorded::default(), &provider, 3_000).await,
        Ok(SubmitReport::default())
    );
    provider.set_mode(SandboxMode::Refuse);
    let report = submit_due(&submissions, &Recorded::default(), &provider, 62_000).await;
    assert_eq!(report.map(|report| report.rejected), Ok(1));
    let refused = submissions.list(tenant(), None, true).await.expect("list");
    assert_eq!(refused[0].state, STATE_REJECTED);
    assert!(refused[0].refusal.is_some());
    assert_eq!(
        submit_due(&submissions, &Recorded::default(), &provider, 10_000_000).await,
        Ok(SubmitReport::default())
    );
}

/// One invoice per `bills`, each issued "number pending" for the vendor (ADR-0251).
fn vendor_log(bills: &[u32]) -> Vec<EventEnvelope<RawPayload>> {
    let mut events = log(0, bills);
    for event in &mut events {
        let mut issued: FiscalInvoiceIssued = event.data.decode().expect("decodes");
        issued.invoice.invoice_sequence = 0;
        issued.invoice.number_source = Open::from_known(EInvoiceNumberSource::Vendor);
        event.data = RawPayload::encode(&issued).expect("a payload");
    }
    events
}

#[tokio::test]
async fn a_pending_invoice_is_numbered_by_the_vendor_and_its_number_written_for_the_store() {
    let events = FakeStore::new();
    append(&events, &vendor_log(&[1, 2])).await;
    let submissions = InMemorySubmissions::default();
    let corrector = corrector("EINVOICE_KIND_ADJUSTMENT").await;
    assert_eq!(
        feed_store(&events, &submissions, &corrector, tenant(), store(), NOW_MS).await,
        Ok(2)
    );
    let recorded = submissions.list(tenant(), None, false).await.expect("list");
    assert!(
        recorded.iter().all(|row| is_pending(&row.invoice_number)),
        "no number is invented before the vendor gives one"
    );

    let written = Recorded::default();
    let provider = SandboxEInvoice::new();
    let report = submit_due(&submissions, &written, &provider, NOW_MS).await;
    assert_eq!(report.map(|report| report.submitted), Ok(2));

    let mut rows = submissions.list(tenant(), None, false).await.expect("list");
    rows.sort_by(|a, b| a.bill_id.cmp(&b.bill_id));
    let numbers: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.state.as_str(), row.invoice_number.as_str()))
        .collect();
    assert_eq!(numbers.len(), 2);
    assert!(
        numbers
            .iter()
            .all(|(state, number)| *state == STATE_SUBMITTED && !is_pending(number))
    );
    assert_ne!(numbers[0].1, numbers[1].1, "one bill, one number");

    let mut learned: Vec<(String, String)> = written
        .0
        .lock()
        .expect("written")
        .iter()
        .map(|(_, at, entry)| {
            assert_eq!(*at, store());
            (
                entry.bill_id.to_string(),
                entry.invoice_number.as_str().to_owned(),
            )
        })
        .collect();
    learned.sort();
    let expected: Vec<(String, String)> = rows
        .iter()
        .map(|row| (row.bill_id.clone(), row.invoice_number.clone()))
        .collect();
    assert_eq!(
        learned, expected,
        "the store learns exactly the numbers recorded"
    );
}

#[tokio::test]
async fn a_void_of_a_pending_invoice_is_corrected_once_the_vendor_has_numbered_it() {
    let events = FakeStore::new();
    let mut batch = vendor_log(&[1]);
    batch.push(voided(1, u128::MAX));
    append(&events, &batch).await;
    let submissions = InMemorySubmissions::default();
    let corrector = corrector("EINVOICE_KIND_ADJUSTMENT").await;
    assert!(
        feed_store(&events, &submissions, &corrector, tenant(), store(), NOW_MS)
            .await
            .is_err(),
        "a correction names the number it corrects, so it waits"
    );
    let provider = SandboxEInvoice::new();
    submit_due(&submissions, &Recorded::default(), &provider, NOW_MS)
        .await
        .expect("numbers the original");
    assert_eq!(
        feed_store(
            &events,
            &submissions,
            &corrector,
            tenant(),
            store(),
            NOW_MS + 1
        )
        .await,
        Ok(1),
        "the void is corrected once the original has its number"
    );
    let rows = submissions.list(tenant(), None, false).await.expect("list");
    let original = rows
        .iter()
        .find(|row| row.corrects_invoice_number.is_none())
        .expect("the original");
    let correction = rows
        .iter()
        .find(|row| row.corrects_invoice_number.is_some())
        .expect("its correction");
    assert_eq!(
        correction.corrects_invoice_number.as_deref(),
        Some(original.invoice_number.as_str())
    );
    assert_eq!(correction.total_minor, -165_000);
}
