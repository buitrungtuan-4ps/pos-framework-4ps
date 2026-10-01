// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The running-check read route (roadmap-v3 slice E5).
//!
//! `GET /api/tables/{id}/check` answers "what does this table owe right now" with the **edge's** own
//! figure. Until this route the operator UI computed the running total itself, from a tax rate
//! hardcoded at 10% — so a store on any other rate, or with more than one tax class, showed the guest
//! one number and settled against another.
//!
//! It is a pure read of [`Edge::check_totals`](crate::app::Edge::check_totals), which runs the same
//! `billing::assemble` the settle path runs, over the same projection and the same session. One
//! calculation, in one place, with the domain as its home ([ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md)):
//! the till displays, it does not decide.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_core::billing::BillTotals;
use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{BillId, OrderId, TableId};
use pos_proto::money::Money;

use crate::app::{AppError, Edge, PreBill};
use crate::http::{bad_request, error_response, parse_ulid};
use crate::printing::{PrintOutcome, Printers, short_reference};

/// What a table owes, as the till shows it.
#[derive(Debug, Serialize)]
pub(crate) struct CheckResponse {
    /// The sum of the order's live line totals, before tax.
    subtotal: Money,
    /// What has been taken off, once a bill is open to take it off. Two figures rather than one,
    /// because the receipt prints them on two lines and for two different reasons — a discount is
    /// a price decision, a comp is food given away. Zero until something is applied, which is every
    /// bill until somebody reduces one.
    discount_total: Money,
    comp_total: Money,
    /// The tax on those lines, each class rounded once by the domain.
    tax_total: Money,
    /// What the guest owes — the figure the bill will settle against.
    total_due: Money,
}

impl From<&BillTotals> for CheckResponse {
    fn from(totals: &BillTotals) -> Self {
        Self {
            subtotal: totals.subtotal,
            discount_total: totals.discount_total,
            comp_total: totals.comp_total,
            tax_total: totals.tax_total,
            total_due: totals.total_due,
        }
    }
}

/// What one bill owes, where it has got to, and which lines it covers.
#[derive(Debug, Serialize)]
pub(crate) struct BillCheckResponse {
    /// `BILL_STATE_OPEN` while it still owes. A settled, voided, split or merged bill owes nothing
    /// more, and the figures are then what its lines came to rather than a sum to collect.
    state: String,
    /// The order lines it covers ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md)
    /// decision 1), so a till can show a guest what their part of a split table is for.
    order_line_ids: Vec<String>,
    /// The same five figures the table and order reads answer with, so a till draws all three the
    /// same way.
    #[serde(flatten)]
    totals: CheckResponse,
}

/// `GET /api/tables/{id}/check` — the running check, assembled by the edge.
pub(crate) async fn read<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(table_id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_ulid(&table_id).map(TableId::new) else {
        return bad_request("a table id is a ULID");
    };
    match edge.check_totals(table_id) {
        Ok(totals) => (StatusCode::OK, Json(CheckResponse::from(&totals))).into_response(),
        // The one real failure is a line whose tax class the store has published no rate for. That is
        // a configuration error, and the till showing it beats the till inventing a number.
        Err(error) => error_response(&error),
    }
}

/// `GET /api/orders/{id}/check` — the running check for one order, table or no table.
///
/// The counter's read. A takeaway order sits on no table
/// ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)), so the table-keyed route above
/// cannot answer for it, and the cashier needs the figure before taking money as much on a counter
/// order as on a floor one. Same [`Edge::order_totals`](crate::app::Edge::order_totals) the settle
/// path assembles from.
pub(crate) async fn read_for_order<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(order_id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_id) = parse_ulid(&order_id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    match edge.order_totals(order_id) {
        Ok(totals) => (StatusCode::OK, Json(CheckResponse::from(&totals))).into_response(),
        Err(error) => error_response(&error),
    }
}

/// `GET /api/bills/{id}/check` — what one bill owes, where it has got to, and the lines it covers.
///
/// The read a split needs ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md)). Once a
/// table's bill is split, the table read answers for every part still open, and the guest at the
/// till is asking about **theirs** — so the till reads the part it is about to settle, by its id.
/// Same [`Edge::bill_totals`](crate::app::Edge::bill_totals) the settle path assembles from.
pub(crate) async fn read_for_bill<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(bill_id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&bill_id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    match edge.bill_check(bill_id) {
        Ok(check) => (
            StatusCode::OK,
            Json(BillCheckResponse {
                state: check.state.as_wire().to_owned(),
                order_line_ids: check
                    .order_line_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                totals: CheckResponse::from(&check.totals),
            }),
        )
            .into_response(),
        Err(error) => error_response(&error),
    }
}

/// What printing pre-bills came to: one outcome per document, in the order they were sent.
///
/// A list because a split table prints one pre-bill per open part, and a printer can take the first
/// and jam on the second; the till says which.
#[derive(Debug, Serialize)]
pub(crate) struct PrintResponse {
    prints: Vec<String>,
}

/// `POST /api/tables/{id}/check/print` — print the table's pre-bill (roadmap-v3 B2.1): one per open
/// part once its bill is split.
///
/// Printing a pre-bill is presenting the bill, so it needs `billing.bill.open`, as opening one does
/// (ADR-0158), on all three print routes.
pub(crate) async fn print_for_table<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(table_id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_ulid(&table_id).map(TableId::new) else {
        return bad_request("a table id is a ULID");
    };
    if let Err(refused) = edge.authorise(actor, Permission::OpenBill) {
        return error_response(&refused);
    }
    let Some(order_id) = edge.order_for_table(table_id) else {
        return error_response(&AppError::NothingToPrint);
    };
    let pre_bills = edge.pre_bills_for_order(order_id);
    print_all(printers.as_deref(), &edge, order_id, pre_bills).await
}

/// `POST /api/orders/{id}/check/print` — the same for an order, table or no table: a counter order
/// the guest wants to see priced before paying.
pub(crate) async fn print_for_order<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(order_id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_id) = parse_ulid(&order_id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    if let Err(refused) = edge.authorise(actor, Permission::OpenBill) {
        return error_response(&refused);
    }
    let pre_bills = edge.pre_bills_for_order(order_id);
    print_all(printers.as_deref(), &edge, order_id, pre_bills).await
}

/// `POST /api/bills/{id}/check/print` — one open bill's pre-bill, for the guest paying that part of
/// a split table.
pub(crate) async fn print_for_bill<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(bill_id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&bill_id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    if let Err(refused) = edge.authorise(actor, Permission::OpenBill) {
        return error_response(&refused);
    }
    let Some(order_id) = edge.order_for_bill(bill_id) else {
        return error_response(&AppError::UnknownBill);
    };
    let pre_bill = edge.pre_bill_for_bill(bill_id).map(|one| vec![one]);
    print_all(printers.as_deref(), &edge, order_id, pre_bill).await
}

/// Prints each pre-bill under the order's short reference — the one its kitchen tickets carry — and
/// says what came of each.
///
/// Nothing to print is a `409 NOTHING_TO_PRINT` rather than an empty success, because a till that
/// pressed the button and got a blank list would tell the server the paper is on its way.
async fn print_all<S>(
    printers: Option<&Arc<Printers>>,
    edge: &Arc<Edge<S>>,
    order_id: OrderId,
    pre_bills: Result<Vec<PreBill>, AppError>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let pre_bills = match pre_bills {
        Ok(pre_bills) if pre_bills.is_empty() => {
            return error_response(&AppError::NothingToPrint);
        }
        Ok(pre_bills) => pre_bills,
        Err(error) => return error_response(&error),
    };
    let reference = short_reference(&order_id.to_string());
    let session = edge.session();
    let mut prints = Vec::with_capacity(pre_bills.len());
    for pre_bill in &pre_bills {
        let outcome = match printers {
            // A fresh id each time: a table that asks twice gets two pieces of paper.
            Some(printers) => {
                printers
                    .print_pre_bill(
                        &session,
                        edge.store_id(),
                        edge.print_job_id(),
                        &reference,
                        pre_bill,
                    )
                    .await
            }
            None => PrintOutcome::NoPrinter,
        };
        prints.push(outcome.as_wire().to_owned());
    }
    Json(PrintResponse { prints }).into_response()
}
