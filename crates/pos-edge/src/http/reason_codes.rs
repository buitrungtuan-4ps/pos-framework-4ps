// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's reason-code read route
//! ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md)).
//!
//! `GET /api/reason-codes` serves the store's managed list from the live
//! [`EdgeSession`](crate::app::EdgeSession) — the framework default until a `reason_codes` node is
//! published, and the published set afterwards.
//!
//! # One read, every picker
//!
//! Each entry carries the actions it is valid for, and the till filters. The alternative — a route
//! per action, or a query parameter — would mean a second round-trip every time a picker opens, on
//! a screen where the operator is already mid-act. The list is small, it is configuration, and the
//! two pickers that need it today (voiding a line, voiding a bill) sit on different screens, so a
//! device reads it once at start alongside the price book and the button plan.
//!
//! The refusal-reason picker on the confirmation queue is served differently, inside
//! [`crate::http::qr`]'s own response: it is the only picker on that screen, and a queue that
//! arrived with no reasons beside it could open a dialog with nothing in it (ADR-0116).
//!
//! # What it leaves out
//!
//! Retired entries. A retired reason stays in the node so a historic event still resolves, and it
//! shows on no picker — filtering here rather than in the till means an old front end cannot offer
//! one by omission. An unrecognised action token is dropped for the same reason: it matches nothing
//! the edge would accept, so offering it would produce a refusal the operator cannot act on.
//!
//! Reason codes are reference data — a code, a name, and the actions it covers. Nothing here
//! identifies a customer or an employee.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;

use crate::app::Edge;

/// One entry a picker may offer: the id an event carries, the code a report groups by, the name in
/// the store's display language, and the actions it may be cited for.
#[derive(Debug, Serialize)]
pub(crate) struct ReasonCodeResponse {
    reason_code_id: String,
    code: String,
    display_name: String,
    /// The `REASON_ACTION_*` tokens this entry declares, so one read serves every picker.
    applies_to: Vec<String>,
}

/// The store's managed list, as the in-store UI reads it.
#[derive(Debug, Serialize)]
pub(crate) struct ReasonCodesResponse {
    reasons: Vec<ReasonCodeResponse>,
}

/// `GET /api/reason-codes` — the active entries of the store's managed list, read from the live
/// session.
pub(crate) async fn list<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    let language = session.display_language.clone().unwrap_or_default();
    let reasons = session
        .reason_codes
        .codes()
        .iter()
        .filter(|code| code.active)
        .map(|code| ReasonCodeResponse {
            reason_code_id: code.id.to_string(),
            code: code.code.as_str().to_owned(),
            display_name: code.localized_name(&language).as_str().to_owned(),
            applies_to: code
                .applies_to
                .iter()
                .filter(|action| !action.is_unrecognised())
                .map(|action| action.known().as_wire().to_owned())
                .collect(),
        })
        .collect();
    (StatusCode::OK, Json(ReasonCodesResponse { reasons })).into_response()
}
