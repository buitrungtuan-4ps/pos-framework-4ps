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
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_core::billing::BillTotals;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{BillId, OrderId, TableId};
use pos_proto::money::Money;

use crate::app::Edge;
use crate::http::{bad_request, error_response, parse_ulid};

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
