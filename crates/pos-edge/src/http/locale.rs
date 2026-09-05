// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's locale read route ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)).
//!
//! `GET /api/locale` serves the money facts the pay screen needs from the live
//! [`EdgeSession`](crate::app::EdgeSession): the currency, the notes a guest can hand over, and what
//! the total rounds to in cash.
//!
//! # Why this exists
//!
//! `ui/src/lib/money.ts` carried a hardcoded table of quick-cash notes keyed by currency, with three
//! rows in it. A store trading in a currency nobody had typed into that file got a till with one
//! button, and the fix was a front-end edit and a release. Which notes a guest carries is a fact
//! about a **country's cash**, and it now arrives with everything else the cloud publishes.
//!
//! Empty until a `locale` node is published, and the front end keeps its own table as the fallback
//! for exactly that window — so a till that has not synced behaves as it did before.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;

use crate::app::Edge;

/// The store's money settings, as the in-store UI reads them.
#[derive(Debug, Serialize)]
pub(crate) struct LocaleResponse {
    /// The currency every figure on this till is denominated in.
    currency_code: String,
    /// The notes a guest hands over, ascending, in minor units. Empty means the exact amount only.
    cash_denominations: Vec<i64>,
    /// What the grand total is rounded to in cash, in minor units, or `null` for no rounding.
    ///
    /// The till does not apply this — `pos_core::billing` does, and the rounding is already inside
    /// the total the check reports. It is here so the screen can *show* the adjustment as its own
    /// line, which is what makes a rounded bill reconcile for the guest reading it.
    cash_rounding_increment: Option<i64>,
    /// Whether the prices on this store's menu already contain their tax
    /// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
    prices_include_tax: bool,
}

/// `GET /api/locale` — the store's published money settings, read from the live session.
pub(crate) async fn settings<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    (
        StatusCode::OK,
        Json(LocaleResponse {
            currency_code: session.currency.as_str().to_owned(),
            cash_denominations: session.cash_denominations.clone(),
            cash_rounding_increment: session.cash_rounding_increment,
            prices_include_tax: session.prices_include_tax,
        }),
    )
        .into_response()
}
