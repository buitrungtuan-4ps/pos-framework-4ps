// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Bill routes: open a bill on a table, open one on an order, settle it (P5, ADR-0093), and print a
//! settled bill's receipt again as a copy (ADR-0164).
//!
//! Settling proves the payments sum **exactly** to what the bill assembles to (ADR-0028) and
//! allocates the gapless per-store receipt number (ADR-0025). The response carries the receipt
//! number and a `print_receipt` flag; the printing itself runs after the commit, over the
//! [`Printers`](crate::printing::Printers) dispatcher the composition layers in
//! ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)) — so a printer that is down
//! never unwinds a bill the guest has already paid, and the response says what actually came out.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::WireEnum;
use pos_proto::ids::{BillId, FeeId, OrderId, OrderLineId, ReasonCodeId, TableId};
use pos_proto::money::Money;
use pos_proto::{Open, PaymentMethod, UnknownEnumValue};

use pos_proto::ids::{DeviceId, EventId};

use crate::app::{Approval, BillView, BuyerDetails, Edge};
use crate::http::check::{CheckFeeLine, CheckResponse, check_fee_lines};
use crate::http::{bad_request, error_response, parse_ulid};
use crate::printing::{DrawerOutcome, PrintOutcome, Printers};

/// One payment a device applies to a bill: how it was paid, what the guest handed over, what was put
/// against the total, and what they left. Change is what is left of `tendered` once
/// `applied_to_bill` and `tip` are taken out of it.
///
/// `method` arrives as an [`Open`] enum so an unrecognised token is a clean rejection rather than a
/// deserialise failure; [`Self::into_payment`] is the domain boundary that refuses an unspecified or
/// unknown method.
#[derive(Debug, Deserialize)]
pub(crate) struct PaymentRequest {
    method: Open<PaymentMethod>,
    tendered: Money,
    applied_to_bill: Money,
    /// The tip on this tender. Optional, so a device that takes no tips — or one built before the
    /// field existed — settles exactly as before. It replaces the request's `tips` list, which
    /// carried tips beside the payments with no correspondence to them (roadmap **B1.3**).
    #[serde(default)]
    tip: Option<Money>,
}

impl PaymentRequest {
    /// Resolves the wire payment into a domain [`Payment`], refusing an unspecified or unrecognised
    /// method — the wire tolerates `UNSPECIFIED`, a real payment does not.
    fn into_payment(self) -> Result<Payment, UnknownEnumValue> {
        Ok(Payment {
            method: self.method.require()?,
            tendered: self.tendered,
            applied_to_bill: self.applied_to_bill,
            // An absent tip is no tip, in the tendered amount's currency — never a zero in some
            // other currency, which the settlement arithmetic would refuse.
            tip: self
                .tip
                .unwrap_or_else(|| Money::zero(self.tendered.currency_code)),
        })
    }
}

/// A settle request: the payments applied, each carrying the tip taken on it (a separate ledger,
/// never part of the total), and — for a B2B sale — who the tax invoice is for.
///
/// No `Debug`: it can carry a buyer, and a derived one would put that person's name into any log
/// line or rejection message that touched the request (`AGENTS.md` §2).
#[derive(Deserialize)]
pub(crate) struct SettleRequest {
    payments: Vec<PaymentRequest>,
    /// The corporate customer the invoice is issued to
    /// ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)). Absent on every ordinary
    /// retail sale, and `#[serde(default)]` so a till built before this field existed settles
    /// exactly as it did.
    #[serde(default)]
    buyer: Option<BuyerRequest>,
}

/// The buyer a till captured for a corporate invoice.
///
/// Personal data, every field of it, so it goes to the store's subject store and never into an
/// event — `Deserialize` only, with no `Debug`, because a derived one would put a buyer's name into
/// the axum rejection message for a malformed body.
#[derive(Deserialize)]
pub(crate) struct BuyerRequest {
    name: String,
    #[serde(default)]
    tax_code: Option<String>,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl BuyerRequest {
    /// Resolves the wire buyer into the application layer's, trimming each field and dropping the
    /// ones left empty — a blank line on a legal document reads as a value somebody forgot to type.
    ///
    /// Returns `None` when the name is blank, because a buyer with no name is not a buyer: the one
    /// field both Japan's qualified invoice and India's Rule 46 require is the name.
    fn into_details(self) -> Option<BuyerDetails> {
        let trimmed = |value: Option<String>| {
            value
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let name = self.name.trim().to_owned();
        if name.is_empty() {
            return None;
        }
        Some(BuyerDetails {
            name,
            tax_code: trimmed(self.tax_code),
            address: trimmed(self.address),
            email: trimmed(self.email),
        })
    }
}

/// A bill as returned to a device after a command.
#[derive(Debug, Serialize)]
pub(crate) struct BillResponse {
    bill_id: String,
    /// The bill's state (`BILL_STATE_OPEN`, `BILL_STATE_SETTLED`, …).
    state: String,
    /// The gapless receipt number, once settled. Never a legal invoice number.
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_due: Option<Money>,
    /// The state the bill's table moved to, absent for a counter order that has no table
    /// (ADR-0093). The UI already treated this as optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    table_state: Option<String>,
    print_receipt: bool,
    /// What came of that print: `PRINTED`, `NO_PRINTER`, `PRINTER_UNAVAILABLE`, `UNPRINTABLE_TEXT`,
    /// or absent when the settle asked for no receipt (ADR-0100). Until this field existed the till
    /// rendered "Printing receipt…" over a store with no printer wired at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt_print: Option<String>,
    /// What came of opening the cash drawer on a settle that took cash: `OPENED`, `NO_DRAWER` or
    /// `DRAWER_UNAVAILABLE` ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)). Absent when no cash was taken, because nothing
    /// was asked of the drawer.
    #[serde(skip_serializing_if = "Option::is_none")]
    drawer_open: Option<String>,
}

impl From<BillView> for BillResponse {
    fn from(view: BillView) -> Self {
        Self {
            bill_id: view.bill_id.to_string(),
            state: view.state.as_wire().to_owned(),
            receipt_number: view.receipt_number,
            total_due: view.total_due,
            table_state: view.table_state.map(|state| state.as_wire().to_owned()),
            print_receipt: view.print_receipt,
            receipt_print: None,
            drawer_open: None,
        }
    }
}

/// `POST /api/tables/{id}/bill` — open a bill on the order the table holds.
pub(crate) async fn open<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_ulid(&id).map(TableId::new) else {
        return bad_request("a table id is a ULID");
    };
    respond(edge.open_bill(actor, table_id).await)
}

/// `POST /api/orders/{id}/bill` — open a bill on an order, table or no table.
///
/// The counter's route, and the one that makes takeaway revenue collectable: a relayed or QR-counter
/// order is tableless by design (ADR-0064), so `/api/tables/{id}/bill` can never reach it
/// ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)). A floor order billed through here
/// still makes its table's `Occupied → AwaitingPayment` move, because the domain decides that from
/// the order, not from which route was called.
pub(crate) async fn open_for_order<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_id) = parse_ulid(&id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    respond(edge.open_bill_for_order(actor, order_id).await)
}

/// `POST /api/bills/{id}/settle` — settle a bill with the applied payments.
pub(crate) async fn settle<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
    Json(request): Json<SettleRequest>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    let Ok(payments) = request
        .payments
        .into_iter()
        .map(PaymentRequest::into_payment)
        .collect::<Result<Vec<Payment>, _>>()
    else {
        return bad_request("a payment method must be a known method");
    };
    // The store must accept every tendered method (ADR-0080, M7). A store with no `tender` node
    // published accepts any known method; once one is published, a payment by a method it does not
    // list is refused before the bill is settled.
    let session = edge.session();
    if !payments
        .iter()
        .all(|payment| session.tender_accepted(payment.method))
    {
        return bad_request("this store does not accept one of those payment methods as tender");
    }
    // The buyer, when this is a B2B sale (ADR-0107). Its registration number is checked for
    // *shape* by the compiled-in country module and never for existence: existence is a call to the
    // authority, and a cashier has to be able to take a corporate customer's number with the line
    // down. A country this build does not carry stores the number unchecked, which is the same
    // posture the cloud takes for a store profile it cannot validate.
    let buyer = request.buyer.and_then(BuyerRequest::into_details);
    if let Some(buyer) = buyer.as_ref()
        && let Some(tax_code) = buyer.tax_code.as_ref()
        && !tax_code_is_well_formed(tax_code)
    {
        return bad_request("that is not a well-formed tax code for this country");
    }

    let outcome = edge
        .settle_bill(actor, bill_id, payments, buyer.as_ref())
        .await;
    let Ok(view) = outcome else {
        return respond(outcome);
    };

    // After the commit, never before: a printer that is down must not unwind a settled bill, and a
    // rolled-back settle must never have printed (ADR-0100, `Edge::settle_bill`).
    let mut response = BillResponse::from(view.clone());
    // The drawer before the paper: the cashier needs the change before the guest needs the receipt.
    if view.open_drawer {
        let opened = match printers.as_deref() {
            Some(printers) => {
                printers
                    .open_drawer(&edge.session(), edge.store_id(), edge.print_job_id())
                    .await
            }
            None => DrawerOutcome::NoDrawer,
        };
        response.drawer_open = Some(opened.as_wire().to_owned());
    }
    if view.print_receipt {
        let printed = print_receipt_for(
            printers.as_deref(),
            &edge,
            actor.device_id,
            &view,
            buyer.as_ref(),
        )
        .await;
        response.receipt_print = Some(printed.as_wire().to_owned());
    }
    Json(response).into_response()
}

/// What a copy's press reports back to the till (ADR-0164).
#[derive(Debug, Serialize)]
struct ReprintResponse {
    bill_id: BillId,
    receipt_number: u64,
    /// Which copy this was, counting from 1.
    copy_number: u32,
    /// What came of the printing, as the settle reports it: `PRINTED`, `NO_PRINTER`, and so on.
    receipt_print: &'static str,
}

/// `POST /api/bills/{id}/receipt/reprint` — print a settled bill's receipt again, as a copy marked
/// COPY under the same number, and count it
/// ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
///
/// The copy is counted when it is asked for (`billing.receipt.reprinted`), then printed, so a
/// printer that is down still leaves the copy on the record, and the answer says what came out.
pub(crate) async fn reprint<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    let copy = match edge.reprint_receipt(actor, bill_id).await {
        Ok(copy) => copy,
        Err(error) => return error_response(&error),
    };
    // The buyer's details are read again from the subject store, never from the log. A record the
    // retention sweep has scrubbed leaves the copy without them, which is what the law asked for.
    let buyer = match copy.buyer_subject_id {
        Some(subject_id) => edge.buyer_of(subject_id).await.ok().flatten(),
        None => None,
    };
    let printed = match printers.as_deref() {
        Some(printers) => {
            // At the printer of the till the copy was asked for at (ADR-0160 decision 4).
            let session = edge.session();
            let till = printers.till_for(&session, actor.device_id).await;
            printers
                .print_receipt_copy(&session, &till, edge.store_id(), &copy, buyer.as_ref())
                .await
        }
        None => PrintOutcome::NoPrinter,
    };
    Json(ReprintResponse {
        bill_id: copy.bill_id,
        receipt_number: copy.receipt_number,
        copy_number: copy.copy_number,
        receipt_print: printed.as_wire(),
    })
    .into_response()
}

/// Whether a buyer's registration number is well formed for the country this binary carries.
///
/// A build with no `country-*` feature carries an empty registry and accepts anything: refusing
/// every corporate invoice because nobody compiled a country in would make the store *less* able to
/// trade than before the field existed. A build that does carry one applies it — format only.
fn tax_code_is_well_formed(tax_code: &str) -> bool {
    crate::countries::registry()
        .modules()
        .all(|module| module.is_valid_tax_code(tax_code))
}

/// Runs the receipt effect at the printer of the till `device` is, and says what came of it.
///
/// A composition with no dispatcher layered in — the fakes-backed example, a route test that does not
/// care — reports `NO_PRINTER`, which is the truth for it: there is nothing to print on.
async fn print_receipt_for<S>(
    printers: Option<&Arc<Printers>>,
    edge: &Arc<Edge<S>>,
    device: DeviceId,
    view: &BillView,
    buyer: Option<&BuyerDetails>,
) -> PrintOutcome
where
    S: EventStore + Send + Sync + 'static,
{
    let (Some(printers), Some(receipt_number), Some(totals)) =
        (printers, view.receipt_number, view.totals.as_ref())
    else {
        return PrintOutcome::NoPrinter;
    };
    // The till's own receipt printer and languages, where its terminal names them (ADR-0160
    // decision 4). The drawer above is the store's whichever till this is.
    let session = edge.session();
    let till = printers.till_for(&session, device).await;
    printers
        .print_receipt(
            &session,
            &till,
            edge.store_id(),
            // The bill's own id as the idempotency key: a settle retried after an ambiguous failure
            // reuses it and the adapter prints once, which is the same promise the receipt number
            // itself makes (ADR-0025).
            EventId::new(view.bill_id.as_ulid()),
            receipt_number,
            &view.lines,
            totals,
            buyer,
        )
        .await
}

/// Maps a bill command outcome to a response.
fn respond(outcome: Result<BillView, crate::app::AppError>) -> Response {
    match outcome {
        Ok(view) => Json(BillResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}

/// A bill void as a till asks for it: the reason from the store's managed list, plus the manager's
/// badge and PIN — which, unlike a line's, are needed every time.
#[derive(Debug, Deserialize)]
pub(crate) struct VoidBillRequest {
    reason_code_id: ReasonCodeId,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

impl VoidBillRequest {
    fn approval(&self) -> Option<Approval> {
        let code = self.approver_code.clone()?;
        let pin = self.approver_pin.clone()?;
        Some(Approval { code, pin })
    }
}

/// A fee waive as a till asks for it: the reason from the store's managed list, plus the badge
/// and PIN of somebody who holds `billing.fee.waive` where the person acting needs one
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 5).
#[derive(Debug, Deserialize)]
pub(crate) struct WaiveFeeRequest {
    reason_code_id: ReasonCodeId,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

impl WaiveFeeRequest {
    fn approval(&self) -> Option<Approval> {
        let code = self.approver_code.clone()?;
        let pin = self.approver_pin.clone()?;
        Some(Approval { code, pin })
    }
}

/// `POST /api/bills/{id}/fees/{fee_id}/waive` — waive one of an open bill's fees, citing a reason
/// (ADR-0159 decision 5).
///
/// Answers with the bill as it now stands, in the shape of its check, so the till redraws it from
/// the edge's arithmetic and never subtracts the fee itself: the fee's tax goes with it. A fee
/// that is not waivable answers `409 FEE_NOT_WAIVABLE`, one the bill does not charge
/// `409 FEE_NOT_ON_BILL`, and a bill that is not open `409 TRANSITION_REFUSED`.
pub(crate) async fn waive_fee<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path((id, fee_id)): Path<(String, String)>,
    Json(request): Json<WaiveFeeRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    let Some(fee_id) = parse_ulid(&fee_id).map(FeeId::new) else {
        return bad_request("a fee id is a ULID");
    };
    let approval = request.approval();
    match edge
        .waive_fee(
            actor,
            bill_id,
            fee_id,
            request.reason_code_id,
            approval.as_ref(),
        )
        .await
    {
        Ok(totals) => Json(CheckResponse::of(&totals, &edge.session())).into_response(),
        Err(error) => error_response(&error),
    }
}

/// The state a voided bill came to rest in, so the till can show it rather than re-reading.
#[derive(Debug, Serialize)]
pub(crate) struct VoidBillResponse {
    state: &'static str,
}

/// A discount as a till asks for it: how much, why, and the manager standing at the till for it.
///
/// The approval is optional in the shape and required by the act today, exactly as a line void's
/// is. No store publishes a discount ceiling, so the domain reads the ceiling as zero and every
/// discount needs `billing.discount.override_ceiling` — but the till sends the same request either
/// way, and the edge answers `403` when the manager is the missing piece. When a ceiling is
/// published, a discount under it will start succeeding without one, with no change here.
#[derive(Debug, Deserialize)]
pub(crate) struct DiscountRequest {
    /// How much comes off, in minor units. Never a percentage: `billing.discount.applied` records
    /// an `amount`, so a percentage would have to be resolved somewhere, and resolving it at the
    /// till would put the arithmetic on the device that does not own the price book (§14.2).
    amount: Money,
    reason_code_id: ReasonCodeId,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

impl DiscountRequest {
    fn approval(&self) -> Option<Approval> {
        let code = self.approver_code.clone()?;
        let pin = self.approver_pin.clone()?;
        Some(Approval { code, pin })
    }
}

/// What the bill comes to once the discount is on it — the edge's own arithmetic, so the till never
/// subtracts a discount from a total itself and arrives at a different tax.
#[derive(Debug, Serialize)]
pub(crate) struct DiscountResponse {
    subtotal: Money,
    discount_total: Money,
    comp_total: Money,
    tax_total: Money,
    total_due: Money,
    /// Each fee on the bill once the discount is on it, as the check reads list them: a
    /// percentage taken after discounts moves with the discount (ADR-0159).
    fee_lines: Vec<CheckFeeLine>,
}

/// `POST /api/bills/{id}/discount` — take money off a bill before it settles (roadmap B2.2).
pub(crate) async fn discount<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<DiscountRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    if request.amount.amount_minor <= 0 {
        // Nothing off is not a discount, and a negative one is a way to add to a bill without
        // selling anything. Both are malformed rather than refused by the domain, in the same
        // class as a path segment that is not a ULID.
        return bad_request("a discount is greater than zero");
    }
    let approval = request.approval();
    match edge
        .discount_bill(
            actor,
            bill_id,
            request.amount,
            request.reason_code_id,
            approval.as_ref(),
        )
        .await
    {
        Ok(totals) => {
            let session = edge.session();
            Json(DiscountResponse {
                subtotal: totals.subtotal,
                discount_total: totals.discount_total,
                comp_total: totals.comp_total,
                tax_total: totals.tax_total,
                total_due: totals.total_due,
                fee_lines: check_fee_lines(&totals, &session),
            })
            .into_response()
        }
        Err(error) => error_response(&error),
    }
}

/// The parts a split proposes, each the order lines one new bill will cover.
#[derive(Debug, Deserialize)]
pub(crate) struct SplitRequest {
    parts: Vec<Vec<OrderLineId>>,
}

/// The bills a split produced, in the order the parts were given.
#[derive(Debug, Serialize)]
pub(crate) struct SplitResponse {
    bill_ids: Vec<String>,
}

/// `POST /api/bills/{id}/split` — partition a bill's lines into two or more bills
/// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md)).
///
/// No manager and no PIN, unlike the void beside it: a split partitions amounts that are already
/// captured and creates, forgives and moves nothing (decision 6). Everything that can refuse it is
/// the domain's — the bill must be open, and the parts must be a partition of what it covers.
pub(crate) async fn split<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<SplitRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    match edge.split_bill(actor, bill_id, request.parts).await {
        Ok(bill_ids) => Json(SplitResponse {
            bill_ids: bill_ids.iter().map(ToString::to_string).collect(),
        })
        .into_response(),
        Err(error) => error_response(&error),
    }
}

/// The bills a merge folds into the target.
#[derive(Debug, Deserialize)]
pub(crate) struct MergeRequest {
    absorbed_bill_ids: Vec<BillId>,
}

/// The surviving bill and everything it now covers.
#[derive(Debug, Serialize)]
pub(crate) struct MergeResponse {
    bill_id: String,
}

/// `POST /api/bills/{id}/merge` — fold other bills into this one, which survives
/// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 5).
///
/// The path names the **target**: it is the bill the cashier is standing in front of, and it keeps
/// its identity. Every absorbed bill must be open and on the same table, which is a floor
/// restriction rather than a model one — settling moves a table, and a bill over two tables makes
/// "which table moved?" a question with two answers — and of the same order, which two counter
/// bills, sharing no table, need too: a settle charges a bill's lines from its own order, so
/// another order's would be charged nothing. Each is refused `409` before anything is written.
pub(crate) async fn merge<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<MergeRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    if request.absorbed_bill_ids.is_empty() {
        // Merging nothing in is not a merge; it is the bill it already was. Malformed rather than
        // refused by the domain, in the same class as a path segment that is not a ULID.
        return bad_request("a merge names at least one bill to fold in");
    }
    match edge
        .merge_bills(actor, bill_id, request.absorbed_bill_ids)
        .await
    {
        Ok(bill_id) => Json(MergeResponse {
            bill_id: bill_id.to_string(),
        })
        .into_response(),
        Err(error) => error_response(&error),
    }
}

/// `POST /api/bills/{id}/void` — void a bill before it settles, citing a reason from the store's
/// managed list (ADR-0115, roadmap B2.2).
///
/// Always needs a manager's PIN: a bill is money whether or not the kitchen started, so unlike a
/// line there is no unfired shape to let through. A settled bill is refused by the bill machine —
/// reversing one is a refund, with its own permission and its own reason.
pub(crate) async fn void<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<VoidBillRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    let approval = request.approval();
    match edge
        .void_bill(actor, bill_id, request.reason_code_id, approval.as_ref())
        .await
    {
        Ok(state) => Json(VoidBillResponse {
            state: state.as_wire(),
        })
        .into_response(),
        Err(error) => error_response(&error),
    }
}
