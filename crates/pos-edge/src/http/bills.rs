// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Bill routes: open a bill on a table, settle it (P5).
//!
//! Settling proves the payments sum **exactly** to what the bill assembles to (ADR-0028) and
//! allocates the gapless per-store receipt number (ADR-0025). The response carries the receipt
//! number and a `print_receipt` flag; the printing itself is the binary's to run after the commit,
//! so the HTTP layer holds no printer either.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::billing::Payment;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{BillId, DeviceId, TableId};
use pos_proto::money::Money;
use pos_proto::{Open, PaymentMethod, UnknownEnumValue};

use crate::app::{BillView, Edge};
use crate::http::auth::device_actor;
use crate::http::{bad_request, error_response, parse_ulid};

/// One payment a device applies to a bill: how it was paid, what the guest handed over, and what was
/// put against the total. Change lives in the gap between `tendered` and `applied_to_bill`.
///
/// `method` arrives as an [`Open`] enum so an unrecognised token is a clean rejection rather than a
/// deserialise failure; [`Self::into_payment`] is the domain boundary that refuses an unspecified or
/// unknown method.
#[derive(Debug, Deserialize)]
pub(crate) struct PaymentRequest {
    method: Open<PaymentMethod>,
    tendered: Money,
    applied_to_bill: Money,
}

impl PaymentRequest {
    /// Resolves the wire payment into a domain [`Payment`], refusing an unspecified or unrecognised
    /// method — the wire tolerates `UNSPECIFIED`, a real payment does not.
    fn into_payment(self) -> Result<Payment, UnknownEnumValue> {
        Ok(Payment {
            method: self.method.require()?,
            tendered: self.tendered,
            applied_to_bill: self.applied_to_bill,
        })
    }
}

/// A settle request: the payments applied, and any tips (a separate ledger, never part of the total).
#[derive(Debug, Deserialize)]
pub(crate) struct SettleRequest {
    payments: Vec<PaymentRequest>,
    #[serde(default)]
    tips: Vec<Money>,
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
    /// The state the bill's table moved to.
    table_state: String,
    print_receipt: bool,
}

impl From<BillView> for BillResponse {
    fn from(view: BillView) -> Self {
        Self {
            bill_id: view.bill_id.to_string(),
            state: view.state.as_wire().to_owned(),
            receipt_number: view.receipt_number,
            total_due: view.total_due,
            table_state: view.table_state.as_wire().to_owned(),
            print_receipt: view.print_receipt,
        }
    }
}

/// `POST /api/tables/{id}/bill` — open a bill on the order the table holds.
pub(crate) async fn open<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(device_id): Extension<DeviceId>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_ulid(&id).map(TableId::new) else {
        return bad_request("a table id is a ULID");
    };
    respond(edge.open_bill(device_actor(device_id), table_id).await)
}

/// `POST /api/bills/{id}/settle` — settle a bill with the applied payments.
pub(crate) async fn settle<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(device_id): Extension<DeviceId>,
    Path(id): Path<String>,
    Json(request): Json<SettleRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
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
    respond(
        edge.settle_bill(device_actor(device_id), bill_id, payments, request.tips)
            .await,
    )
}

/// Maps a bill command outcome to a response.
fn respond(outcome: Result<BillView, crate::app::AppError>) -> Response {
    match outcome {
        Ok(view) => Json(BillResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
