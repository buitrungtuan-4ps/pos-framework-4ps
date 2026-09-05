// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Bill routes: open a bill on a table, open one on an order, settle it (P5, ADR-0093).
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
use pos_proto::ids::{BillId, OrderId, TableId};
use pos_proto::money::Money;
use pos_proto::{Open, PaymentMethod, UnknownEnumValue};

use pos_proto::ids::EventId;

use crate::app::{BillView, BuyerDetails, Edge};
use crate::http::{bad_request, error_response, parse_ulid};
use crate::printing::{PrintOutcome, Printers};

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
    if view.print_receipt {
        let printed = print_receipt_for(printers.as_deref(), &edge, &view, buyer.as_ref()).await;
        response.receipt_print = Some(printed.as_wire().to_owned());
    }
    Json(response).into_response()
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

/// Runs the receipt effect and says what came of it.
///
/// A composition with no dispatcher layered in — the fakes-backed example, a route test that does not
/// care — reports `NO_PRINTER`, which is the truth for it: there is nothing to print on.
async fn print_receipt_for<S>(
    printers: Option<&Arc<Printers>>,
    edge: &Arc<Edge<S>>,
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
    printers
        .print_receipt(
            &edge.session(),
            edge.store_id(),
            // The bill's own id as the idempotency key: a settle retried after an ambiguous failure
            // reuses it and the adapter prints once, which is the same promise the receipt number
            // itself makes (ADR-0025).
            EventId::new(view.bill_id.as_ulid()),
            receipt_number,
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
