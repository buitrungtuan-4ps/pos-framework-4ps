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

use pos_ports::event_store::EventStore;
use pos_proto::ids::{OrderId, TableId};
use pos_proto::money::Money;

use crate::app::Edge;
use crate::http::{bad_request, error_response, parse_ulid};

/// What a table owes, as the till shows it.
#[derive(Debug, Serialize)]
pub(crate) struct CheckResponse {
    /// The sum of the order's live line totals, before tax.
    subtotal: Money,
    /// The tax on those lines, each class rounded once by the domain.
    tax_total: Money,
    /// What the guest owes — the figure the bill will settle against.
    total_due: Money,
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
        Ok(totals) => (
            StatusCode::OK,
            Json(CheckResponse {
                subtotal: totals.subtotal,
                tax_total: totals.tax_total,
                total_due: totals.total_due,
            }),
        )
            .into_response(),
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
        Ok(totals) => (
            StatusCode::OK,
            Json(CheckResponse {
                subtotal: totals.subtotal,
                tax_total: totals.tax_total,
                total_due: totals.total_due,
            }),
        )
            .into_response(),
        Err(error) => error_response(&error),
    }
}
