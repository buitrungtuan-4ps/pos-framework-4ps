// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the store has taken today, for the Today screen's tile
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2).
//!
//! One read, `GET /api/reports/takings`: the sum of the bills settled in the current business day
//! and how many there were. The store's figure and nothing finer — no person, no till, no shift —
//! because takings per person would be monitoring staff, which needs a legal basis this read does
//! not have.
//!
//! It needs a signed-in person whose own role grants `reports.takings.view`, whether or not the
//! store enforces each person's own permissions, as retiring a device does. Takings are confidential,
//! and a store that does not enforce yet had no takings read for anyone to keep, so there is nothing
//! the store-wide set would be preserving.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::BusinessDate;
use pos_proto::money::Money;

use crate::app::Edge;
use crate::http::{ERROR_REASON_HEADER, error_response};

/// What the store has taken today, as the Today screen's tile shows it.
#[derive(Debug, Serialize)]
pub(crate) struct TakingsResponse {
    /// The business day the figure is for.
    business_date: BusinessDate,
    /// What the day's settled bills came to: what their guests paid.
    takings_amount: Money,
    /// How many bills settled that day.
    bill_count: u32,
}

/// `GET /api/reports/takings` — what the store has taken in the current business day.
///
/// `403` with `PERMISSION_DENIED` for a person whose own role does not grant
/// `reports.takings.view`: the device is paired and somebody is signed in, so the answer is about
/// standing, not about signing in again.
pub(crate) async fn takings<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    if !edge
        .session()
        .staff
        .grants(actor.employee_id, Permission::ViewTakings)
    {
        return (
            StatusCode::FORBIDDEN,
            [(ERROR_REASON_HEADER, "PERMISSION_DENIED")],
            "seeing today's takings needs reports.takings.view",
        )
            .into_response();
    }
    match edge.takings_today() {
        Ok(takings) => Json(TakingsResponse {
            business_date: takings.business_date,
            takings_amount: takings.takings_amount,
            bill_count: takings.bill_count,
        })
        .into_response(),
        Err(error) => error_response(&error),
    }
}
