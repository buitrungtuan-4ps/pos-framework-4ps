// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A card taken through the card terminal
//! ([ADR-0230](../../../../docs/adr/0230-a-card-is-taken-through-the-terminal-port-and-an-unknown-result-parks-the-bill.md)):
//! `POST /api/bills/{id}/card` asks the terminal, `GET /api/bills/{id}/card` reads the attempt a
//! bill is parked on, and `POST /api/bills/{id}/card/{payment_id}/resolve` looks it up, confirms it
//! or cancels it. No card data is sent, held or answered: only the terminal's outcome.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::PaymentMethod;
use pos_proto::ids::{BillId, PaymentId};
use pos_proto::money::Money;

use crate::app::{CardAsk, CardOutcome, CardRefusal, CardResolution, Edge, TillScope};
use crate::http::bills::{BillResponse, print_receipt_for};
use crate::http::{bad_request, error_response, parse_ulid, shifts};
use crate::printing::Printers;

/// The most equal shares an even split records (ADR-0220 decision 3).
const SPLIT_WAYS: std::ops::RangeInclusive<u8> = 2..=20;

/// A card a till asks the terminal to take.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct CardRequest {
    /// What it applies to the bill.
    amount: Money,
    /// The tip on it, taken on the terminal with the amount.
    #[serde(default)]
    tip: Option<Money>,
    /// How many equal shares an even split divided the bill into, 2 to 20, when it is one of them.
    #[serde(default)]
    split_ways: Option<u8>,
}

/// How a person resolves a parked card.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct ResolveRequest {
    /// `LOOK_UP`, `CONFIRM` or `CANCEL`.
    action: String,
    /// A manager's badge code, for a `CONFIRM` the store asks a manager to approve (ADR-0243).
    #[serde(default)]
    approver_code: Option<String>,
    /// That manager's PIN, sent once and never stored.
    #[serde(default)]
    approver_pin: Option<String>,
}

/// What came of a card.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct CardResponse {
    bill_id: BillId,
    payment_id: PaymentId,
    /// `PAYMENT_OUTCOME_CAPTURED`, `PAYMENT_OUTCOME_DECLINED` or `PAYMENT_OUTCOME_UNKNOWN`: the
    /// last parks the bill.
    outcome: &'static str,
    /// What the bill's payments now come to and what remains, after a capture.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    paid_total: Option<Money>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    remaining: Option<Money>,
    /// The settled bill, when the capture covered what remained.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    settled: Option<BillResponse>,
}

/// The attempt a bill is parked on.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct ParkedCardResponse {
    bill_id: BillId,
    payment_id: PaymentId,
    amount: Money,
    tip: Money,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/bills/{id}/card",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("billing.payment.take"))),
    params(("id" = String, Path, description = "The bill's id, a ULID")),
    request_body(content = CardRequest, description = "`amount`, with an optional `tip` and `split_ways`", content_type = "application/json"),
    responses(
        (status = 200, body = CardResponse, description = "The terminal answered: captured (and recorded, settling the bill when it \
                                      covered it), declined (nothing recorded) or unknown (the bill is parked)"),
        (status = 400, description = "The id is not a ULID, `split_ways` is not 2 to 20, or the store takes no card"),
        (status = 401, description = "This device is not paired"),
        (status = 403, description = "Nobody is signed in on this device, or a kiosk outside its own order"),
        (status = 409, description = "No terminal (`CARD_TERMINAL_NONE`), a parked bill (`CARD_RESULT_UNKNOWN`), \
                                      more than the bill owes, or the refusals a payment makes"),
        (status = 503, description = "The terminal was not reached (`CARD_TERMINAL_UNAVAILABLE`); nothing was recorded"),
    ),
    tag = "bills",
))]
/// `POST /api/bills/{id}/card` — take a card through the terminal (ADR-0230 decision 2).
pub(crate) async fn take_card<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
    Json(request): Json<CardRequest>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    if request
        .split_ways
        .is_some_and(|ways| !SPLIT_WAYS.contains(&ways))
    {
        return bad_request("an even split is two to twenty shares");
    }
    if !edge.session().tender_accepted(PaymentMethod::Card) {
        return bad_request("this store does not accept a card as tender");
    }
    let ask = CardAsk {
        amount: request.amount,
        tip: request
            .tip
            .unwrap_or_else(|| Money::zero(request.amount.currency_code)),
        split_ways: request.split_ways,
    };
    let scope = till_scope(&edge, printers.as_deref(), actor).await;
    match edge.take_card_at(actor, scope, bill_id, ask).await {
        Ok(outcome) => answer(&edge, printers.as_deref(), actor, bill_id, outcome).await,
        Err(error) => error_response(&error),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/bills/{id}/card",
    security(("till_session" = [])),
    params(("id" = String, Path, description = "The bill's id, a ULID")),
    responses(
        (status = 200, body = ParkedCardResponse, description = "The card attempt this bill is parked on"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired"),
        (status = 403, description = "Nobody is signed in on this device"),
        (status = 404, description = "The bill is not parked on a card"),
    ),
    tag = "bills",
))]
/// `GET /api/bills/{id}/card` — the card a bill is parked on, which a till that reloads shows amber.
pub(crate) async fn parked_card<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    match edge.parked_card(bill_id) {
        Some(card) => Json(ParkedCardResponse {
            bill_id,
            payment_id: card.payment_id,
            amount: card.amount,
            tip: card.tip,
        })
        .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/bills/{id}/card/{payment_id}/resolve",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("billing.payment.take"))),
    params(
        ("id" = String, Path, description = "The bill's id, a ULID"),
        ("payment_id" = String, Path, description = "The parked attempt's id, a ULID"),
    ),
    request_body(content = ResolveRequest, description = "`action`: `LOOK_UP`, `CONFIRM` or `CANCEL`", content_type = "application/json"),
    responses(
        (status = 200, body = CardResponse, description = "Captured (recorded), declined (the park ends) or still unknown"),
        (status = 400, description = "An id is not a ULID, or the action is not one of the three"),
        (status = 401, description = "This device is not paired"),
        (status = 403, description = "Nobody is signed in on this device"),
        (status = 409, description = "The bill is not parked on that card (`CARD_NOT_PARKED`), a cancel of a card the \
                                      terminal took (`CARD_ALREADY_CAPTURED`), no terminal, a `CONFIRM` without the \
                                      manager `billing.card_confirm_approval` asks for (`APPROVAL_REQUIRED`, \
                                      `APPROVAL_REFUSED`, ADR-0243), or the refusals a payment makes"),
        (status = 503, description = "The terminal was not reached; nothing changed"),
    ),
    tag = "bills",
))]
/// `POST /api/bills/{id}/card/{payment_id}/resolve` — resolve a parked card (ADR-0230 decision 3).
pub(crate) async fn resolve_card<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path((id, payment)): Path<(String, String)>,
    Json(request): Json<ResolveRequest>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let (Some(bill_id), Some(payment_id)) = (
        parse_ulid(&id).map(BillId::new),
        parse_ulid(&payment).map(PaymentId::new),
    ) else {
        return bad_request("a bill id and a payment id are ULIDs");
    };
    let resolution = match request.action.as_str() {
        "LOOK_UP" => CardResolution::LookUp,
        "CONFIRM" => CardResolution::Confirm,
        "CANCEL" => CardResolution::Cancel,
        _ => return bad_request("the action is LOOK_UP, CONFIRM or CANCEL"),
    };
    let approval = request
        .approver_code
        .zip(request.approver_pin)
        .map(|(code, pin)| crate::app::Approval { code, pin });
    let scope = till_scope(&edge, printers.as_deref(), actor).await;
    match edge
        .resolve_card_with(
            actor,
            scope,
            (bill_id, payment_id),
            resolution,
            approval.as_ref(),
        )
        .await
    {
        Ok(outcome) => answer(&edge, printers.as_deref(), actor, bill_id, outcome).await,
        Err(error) => error_response(&error),
    }
}

/// A refused card's answer: `503` where the terminal, not the store, was out of reach and nothing
/// was recorded, and `409` for every other refusal (ADR-0230).
pub(crate) fn refusal_response(card: CardRefusal, reason: &'static str) -> Response {
    let status = if card == CardRefusal::TerminalUnavailable {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::CONFLICT
    };
    super::refusal(status, reason, card.to_string())
}

/// The till a device is, for the shift a card's payment is stamped with. A card goes into no
/// drawer, so a till that cannot be read is no till's, as for any tender but cash.
async fn till_scope<S>(
    edge: &Arc<Edge<S>>,
    printers: Option<&Arc<Printers>>,
    actor: Actor,
) -> TillScope
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    match shifts::till_scope(edge, printers, actor, None).await {
        Ok(scope) => scope,
        Err(error) => {
            tracing::warn!(%error, "the till a device is could not be read; its card is no till's");
            TillScope::default()
        }
    }
}

/// The answer to a card, printing the receipt of one that settled the bill.
async fn answer<S>(
    edge: &Arc<Edge<S>>,
    printers: Option<&Arc<Printers>>,
    actor: Actor,
    bill_id: BillId,
    outcome: CardOutcome,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let response = match outcome {
        CardOutcome::Captured { payment_id, taken } => {
            let mut settled = None;
            if let Some(view) = taken.settled {
                let mut response = BillResponse::from(view.clone());
                if view.print_receipt {
                    let printed =
                        print_receipt_for(printers, edge, actor.device_id, &view, None).await;
                    response.receipt_print = Some(printed.as_wire().to_owned());
                }
                settled = Some(response);
            }
            CardResponse {
                bill_id,
                payment_id,
                outcome: "PAYMENT_OUTCOME_CAPTURED",
                paid_total: Some(taken.paid_total),
                remaining: Some(taken.remaining),
                settled,
            }
        }
        CardOutcome::Declined { payment_id } => CardResponse {
            bill_id,
            payment_id,
            outcome: "PAYMENT_OUTCOME_DECLINED",
            paid_total: None,
            remaining: None,
            settled: None,
        },
        CardOutcome::Unknown { payment_id } => CardResponse {
            bill_id,
            payment_id,
            outcome: "PAYMENT_OUTCOME_UNKNOWN",
            paid_total: None,
            remaining: None,
            settled: None,
        },
    };
    Json(response).into_response()
}
