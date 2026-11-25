// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The submitter, fed from the event log and kept in a table
//! ([ADR-0241](../../../../docs/adr/0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md)).
//!
//! Each tick reads every active store's log after the cursor it keeps, records each
//! `fiscal.invoice.issued` once as `PENDING`, then submits a bounded page of due invoices through
//! the provider with #1350's retry rule ([`super::submitter`]): a failure that may pass is retried
//! with a doubling wait, a refusal stops. Nothing personal is kept: the buyer is a subject id.

use core::future::Future;
use core::num::NonZeroU32;
use core::pin::Pin;
use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::fiscalization::{
    CorrectionKind, Fiscalization, InvoiceCorrection, InvoiceNumber, InvoiceRequest,
};
use pos_proto::determinism::ClockSource;
use pos_proto::einvoice::EInvoiceKind;
use pos_proto::events::{EventType, FiscalInvoiceIssued};
use pos_proto::ids::{BillId, EventId, StoreId, TenantId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::time::CalendarDate;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum as _;
use store_postgres::SubmissionRow;

use super::submitter::{retry_wait_seconds, retryable};
use crate::dashboard::projector::StoreCatalog;

/// Events one page reads from a store's log.
pub const PAGE: u32 = 500;
/// Pages one tick reads from one store, so a store far behind cannot starve the rest.
pub const PAGES_PER_TICK: u32 = 20;
/// Invoices one tick submits.
pub const SUBMIT_PER_TICK: i64 = 200;
/// Invoices one list read returns.
pub const LIST_LIMIT: i64 = 1_000;
/// How often the loop ticks.
pub const INTERVAL: Duration = Duration::from_secs(30);

/// A store's failure, as text for the log.
pub type FeedError = String;
/// What a store call returns.
pub type FeedFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, FeedError>> + Send + 'a>>;

/// The submissions and the feed's cursors: a table in `store-postgres`, or
/// [`InMemorySubmissions`] in tests.
pub trait SubmissionStore: Send + Sync + core::fmt::Debug {
    /// The last event of the store's log the feed has read.
    fn cursor(&self, tenant: TenantId, store: StoreId) -> FeedFuture<'_, Option<EventId>>;
    /// Moves the store's cursor.
    fn advance(&self, tenant: TenantId, store: StoreId, last: EventId) -> FeedFuture<'_, ()>;
    /// Records a submission unless its invoice is recorded; `true` when it was new.
    fn record(&self, row: SubmissionRow) -> FeedFuture<'_, bool>;
    /// Up to `limit` due submissions across the fleet.
    fn due(&self, now_ms: i64, limit: i64) -> FeedFuture<'_, Vec<SubmissionRow>>;
    /// Writes a submission's outcome.
    fn settle(&self, row: SubmissionRow) -> FeedFuture<'_, ()>;
    /// A tenant's submissions, newest first.
    fn list(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        unsubmitted_only: bool,
    ) -> FeedFuture<'_, Vec<SubmissionRow>>;
    /// One submission.
    fn get(&self, tenant: TenantId, invoice_id: Ulid) -> FeedFuture<'_, Option<SubmissionRow>>;
}

fn text<E: core::fmt::Display>(error: E) -> FeedError {
    error.to_string()
}

impl SubmissionStore for store_postgres::PostgresEInvoiceSubmissions {
    fn cursor(&self, tenant: TenantId, store: StoreId) -> FeedFuture<'_, Option<EventId>> {
        Box::pin(async move {
            let last = Self::cursor(self, &tenant.to_string(), &store.to_string())
                .await
                .map_err(text)?;
            Ok(last
                .and_then(|id| id.parse::<Ulid>().ok())
                .map(EventId::new))
        })
    }

    fn advance(&self, tenant: TenantId, store: StoreId, last: EventId) -> FeedFuture<'_, ()> {
        Box::pin(async move {
            Self::advance(
                self,
                &tenant.to_string(),
                &store.to_string(),
                &last.to_string(),
            )
            .await
            .map_err(text)
        })
    }

    fn record(&self, row: SubmissionRow) -> FeedFuture<'_, bool> {
        Box::pin(async move { Self::record(self, &row).await.map_err(text) })
    }

    fn due(&self, now_ms: i64, limit: i64) -> FeedFuture<'_, Vec<SubmissionRow>> {
        Box::pin(async move { Self::due(self, now_ms, limit).await.map_err(text) })
    }

    fn settle(&self, row: SubmissionRow) -> FeedFuture<'_, ()> {
        Box::pin(async move { Self::settle(self, &row).await.map_err(text) })
    }

    fn list(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        unsubmitted_only: bool,
    ) -> FeedFuture<'_, Vec<SubmissionRow>> {
        Box::pin(async move {
            let store = store.map(|store| store.to_string());
            Self::list(
                self,
                &tenant.to_string(),
                store.as_deref(),
                unsubmitted_only,
                LIST_LIMIT,
            )
            .await
            .map_err(text)
        })
    }

    fn get(&self, tenant: TenantId, invoice_id: Ulid) -> FeedFuture<'_, Option<SubmissionRow>> {
        Box::pin(async move {
            Self::get(self, &tenant.to_string(), &invoice_id.to_string())
                .await
                .map_err(text)
        })
    }
}

/// The in-memory rows, by tenant and invoice id.
type Rows = BTreeMap<(String, String), SubmissionRow>;

/// The most submissions [`InMemorySubmissions`] holds.
pub const IN_MEMORY_LIMIT: usize = 10_000;

/// Submissions in memory, bounded at [`IN_MEMORY_LIMIT`], for tests.
#[derive(Debug, Clone, Default)]
pub struct InMemorySubmissions {
    rows: Arc<Mutex<Rows>>,
    cursors: Arc<Mutex<BTreeMap<(TenantId, StoreId), EventId>>>,
}

impl InMemorySubmissions {
    fn rows(&self) -> Result<std::sync::MutexGuard<'_, Rows>, FeedError> {
        self.rows.lock().map_err(|_| "poisoned".to_owned())
    }
}

impl SubmissionStore for InMemorySubmissions {
    fn cursor(&self, tenant: TenantId, store: StoreId) -> FeedFuture<'_, Option<EventId>> {
        let last = self
            .cursors
            .lock()
            .map(|cursors| cursors.get(&(tenant, store)).copied())
            .map_err(|_| "poisoned".to_owned());
        Box::pin(async move { last })
    }

    fn advance(&self, tenant: TenantId, store: StoreId, last: EventId) -> FeedFuture<'_, ()> {
        let done = self
            .cursors
            .lock()
            .map(|mut cursors| {
                cursors.insert((tenant, store), last);
            })
            .map_err(|_| "poisoned".to_owned());
        Box::pin(async move { done })
    }

    fn record(&self, row: SubmissionRow) -> FeedFuture<'_, bool> {
        let done = self.rows().and_then(|mut rows| {
            let key = (row.tenant_id.clone(), row.invoice_id.clone());
            if rows.contains_key(&key) {
                return Ok(false);
            }
            if rows.len() >= IN_MEMORY_LIMIT {
                return Err("full".to_owned());
            }
            rows.insert(key, row);
            Ok(true)
        });
        Box::pin(async move { done })
    }

    fn due(&self, now_ms: i64, limit: i64) -> FeedFuture<'_, Vec<SubmissionRow>> {
        let due = self.rows().map(|rows| {
            let mut due: Vec<SubmissionRow> = rows
                .values()
                .filter(|row| is_open(&row.state) && row.next_attempt_ms <= now_ms)
                .cloned()
                .collect();
            due.sort_by(|a, b| {
                (a.next_attempt_ms, &a.invoice_id).cmp(&(b.next_attempt_ms, &b.invoice_id))
            });
            due.truncate(usize::try_from(limit).unwrap_or(0));
            due
        });
        Box::pin(async move { due })
    }

    fn settle(&self, row: SubmissionRow) -> FeedFuture<'_, ()> {
        let done = self.rows().map(|mut rows| {
            rows.insert((row.tenant_id.clone(), row.invoice_id.clone()), row);
        });
        Box::pin(async move { done })
    }

    fn list(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        unsubmitted_only: bool,
    ) -> FeedFuture<'_, Vec<SubmissionRow>> {
        let (tenant, store) = (tenant.to_string(), store.map(|store| store.to_string()));
        let list = self.rows().map(|rows| {
            let mut list: Vec<SubmissionRow> = rows
                .values()
                .filter(|row| row.tenant_id == tenant)
                .filter(|row| store.as_ref().is_none_or(|store| row.store_id == *store))
                .filter(|row| !unsubmitted_only || row.state != STATE_SUBMITTED)
                .cloned()
                .collect();
            list.sort_by(|a, b| {
                (b.recorded_at_ms, &b.invoice_id).cmp(&(a.recorded_at_ms, &a.invoice_id))
            });
            list
        });
        Box::pin(async move { list })
    }

    fn get(&self, tenant: TenantId, invoice_id: Ulid) -> FeedFuture<'_, Option<SubmissionRow>> {
        let row = self.rows().map(|rows| {
            rows.get(&(tenant.to_string(), invoice_id.to_string()))
                .cloned()
        });
        Box::pin(async move { row })
    }
}

/// Waiting for its first attempt.
pub const STATE_PENDING: &str = "PENDING";
/// Failed for a reason that may pass.
pub const STATE_RETRYING: &str = "RETRYING";
/// The provider has it.
pub const STATE_SUBMITTED: &str = "SUBMITTED";
/// The provider refused it.
pub const STATE_REJECTED: &str = "REJECTED";

fn is_open(state: &str) -> bool {
    state == STATE_PENDING || state == STATE_RETRYING
}

/// `YYYY-MM-DD`.
#[must_use]
pub fn date_text(date: CalendarDate) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

fn parse_date(text: &str) -> Option<CalendarDate> {
    let mut parts = text.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    CalendarDate::from_ymd(year, month, day).ok()
}

/// The row a `fiscal.invoice.issued` records, `PENDING` and due at once.
#[must_use]
pub fn issued_row(
    tenant: TenantId,
    store: StoreId,
    event_id: EventId,
    issued: &FiscalInvoiceIssued,
    now_ms: i64,
) -> SubmissionRow {
    let invoice = &issued.invoice;
    SubmissionRow {
        tenant_id: tenant.to_string(),
        invoice_id: event_id.to_string(),
        store_id: store.to_string(),
        bill_id: issued.bill_id.to_string(),
        invoice_number: format!(
            "{}/{:08}",
            invoice.invoice_symbol.as_str(),
            invoice.invoice_sequence
        ),
        kind: invoice
            .kind
            .known()
            .as_wire()
            .trim_start_matches("EINVOICE_KIND_")
            .to_owned(),
        issued_on: date_text(invoice.issued_on),
        total_minor: invoice.total.amount_minor,
        currency: invoice.total.currency_code.as_str().to_owned(),
        buyer_subject_id: invoice.buyer_subject_id.map(|subject| subject.to_string()),
        connection_id: invoice.connection_id.map(|id| id.to_string()),
        corrects_invoice_number: None,
        state: STATE_PENDING.to_owned(),
        failed_attempts: 0,
        next_attempt_ms: now_ms,
        authority_reference: None,
        refusal: None,
        recorded_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

/// Reads one store's log after its cursor, records each issued invoice, and moves the cursor.
/// Returns how many invoices were new.
///
/// # Errors
///
/// The log or the table could not be read or written; the cursor stays where it was.
pub async fn feed_store<E: EventStore>(
    events: &E,
    submissions: &dyn SubmissionStore,
    tenant: TenantId,
    store: StoreId,
    now_ms: i64,
) -> Result<u32, FeedError> {
    let page = NonZeroU32::new(PAGE).ok_or("page")?;
    let mut cursor = submissions.cursor(tenant, store).await?;
    let mut recorded = 0_u32;
    for _ in 0..PAGES_PER_TICK {
        let mut query = EventQuery::first(store, page);
        if let Some(after) = cursor {
            query = query.after(after);
        }
        let read = events.read(&query).await.map_err(text)?;
        let Some(last) = read.last().map(|event| event.event_id) else {
            break;
        };
        for event in &read {
            if event.event_type.as_str() != EventType::FiscalInvoiceIssued.as_str() {
                continue;
            }
            let Ok(issued) = event.data.decode::<FiscalInvoiceIssued>() else {
                tracing::warn!(store = %store, event_id = %event.event_id, "an issued invoice did not decode; it is not submitted");
                continue;
            };
            let row = issued_row(tenant, store, event.event_id, &issued, now_ms);
            if submissions.record(row).await? {
                recorded = recorded.saturating_add(1);
            }
        }
        submissions.advance(tenant, store, last).await?;
        cursor = Some(last);
        if read.len() < PAGE as usize {
            break;
        }
    }
    Ok(recorded)
}

/// The request a stored submission makes: the store's own number, no lines and no buyer. The
/// cloud holds neither until a vendor's field set is chosen (ADR-0225 R11, to be confirmed).
fn request_of(row: &SubmissionRow) -> Option<InvoiceRequest> {
    let bill = row.bill_id.parse::<Ulid>().ok().map(BillId::new)?;
    let store = row.store_id.parse::<Ulid>().ok().map(StoreId::new)?;
    let currency = CurrencyCode::parse(&row.currency).ok()?;
    let correction = row
        .corrects_invoice_number
        .as_ref()
        .map(|corrected| InvoiceCorrection {
            kind: if row.kind == "REPLACEMENT" {
                CorrectionKind::Replacement
            } else {
                CorrectionKind::Adjustment
            },
            corrects: InvoiceNumber::new(corrected.as_str()),
        });
    Some(InvoiceRequest {
        bill_id: bill,
        store_id: store,
        issued_on: parse_date(&row.issued_on)?,
        buyer: None,
        lines: Vec::new(),
        total: Money::new(currency, row.total_minor),
        issued_number: Some(InvoiceNumber::new(row.invoice_number.as_str())),
        correction,
    })
}

/// What one submission pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubmitReport {
    /// Accepted.
    pub submitted: u32,
    /// Refused.
    pub rejected: u32,
    /// Failed for now.
    pub retrying: u32,
}

/// Submits every due invoice once, at most [`SUBMIT_PER_TICK`], and writes each outcome.
///
/// # Errors
///
/// The table could not be read or written.
pub async fn submit_due<F: Fiscalization>(
    submissions: &dyn SubmissionStore,
    provider: &F,
    now_ms: i64,
) -> Result<SubmitReport, FeedError> {
    let mut report = SubmitReport::default();
    for mut row in submissions.due(now_ms, SUBMIT_PER_TICK).await? {
        row.updated_at_ms = now_ms;
        let Some(request) = request_of(&row) else {
            STATE_REJECTED.clone_into(&mut row.state);
            row.refusal = Some("the stored invoice could not be read back".to_owned());
            report.rejected = report.rejected.saturating_add(1);
            submissions.settle(row).await?;
            continue;
        };
        match provider.issue(&request).await {
            Ok(issued) => {
                STATE_SUBMITTED.clone_into(&mut row.state);
                row.authority_reference = issued.authority_reference.map(String::from);
                report.submitted = report.submitted.saturating_add(1);
            }
            Err(error) if retryable(error.status()) => {
                row.failed_attempts = row.failed_attempts.saturating_add(1);
                let wait = retry_wait_seconds(u32::try_from(row.failed_attempts).unwrap_or(0));
                STATE_RETRYING.clone_into(&mut row.state);
                row.next_attempt_ms = now_ms.saturating_add(wait.saturating_mul(1_000));
                report.retrying = report.retrying.saturating_add(1);
            }
            Err(error) => {
                row.failed_attempts = row.failed_attempts.saturating_add(1);
                STATE_REJECTED.clone_into(&mut row.state);
                row.refusal = Some(error.to_string());
                report.rejected = report.rejected.saturating_add(1);
            }
        }
        submissions.settle(row).await?;
    }
    Ok(report)
}

/// One pass: feeds every active store, then submits what is due. A store that fails is logged and
/// skipped.
///
/// # Errors
///
/// The fleet could not be listed, or the table could not be read or written.
pub async fn pass<E, K, F>(
    events: &E,
    catalog: &K,
    submissions: &dyn SubmissionStore,
    provider: &F,
    now_ms: i64,
) -> Result<SubmitReport, FeedError>
where
    E: EventStore,
    K: StoreCatalog,
    F: Fiscalization,
{
    let stores = catalog.active_stores().await.map_err(text)?;
    for (tenant, store) in stores {
        if let Err(error) = feed_store(events, submissions, tenant, store, now_ms).await {
            tracing::warn!(%error, store = %store, "the e-invoice feed could not read a store this pass");
        }
    }
    submit_due(submissions, provider, now_ms).await
}

/// Runs [`pass`] every [`INTERVAL`] until `shutdown`.
pub async fn run<E, K, F, C>(
    events: E,
    catalog: K,
    submissions: Arc<dyn SubmissionStore>,
    provider: F,
    clock: C,
    shutdown: impl Future<Output = ()>,
) where
    E: EventStore,
    K: StoreCatalog,
    F: Fiscalization,
    C: ClockSource,
{
    tokio::pin!(shutdown);
    loop {
        let now_ms = clock.now().as_milliseconds_since_epoch();
        match pass(&events, &catalog, submissions.as_ref(), &provider, now_ms).await {
            Ok(report) if report != SubmitReport::default() => {
                tracing::info!(?report, "e-invoice submission pass");
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "the e-invoice submission pass failed"),
        }
        tokio::select! {
            () = &mut shutdown => return,
            () = tokio::time::sleep(INTERVAL) => {}
        }
    }
}

/// The kind token a stored row carries, for a correction.
#[must_use]
pub fn kind_token(kind: EInvoiceKind) -> &'static str {
    match kind {
        EInvoiceKind::Replacement => "REPLACEMENT",
        EInvoiceKind::Adjustment => "ADJUSTMENT",
        EInvoiceKind::Unspecified | EInvoiceKind::Original => "ORIGINAL",
    }
}

#[cfg(test)]
mod tests;
