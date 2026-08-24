// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/pair` — redeem a pairing code for a device token (ADR-0030).
//!
//! The human-facing QR/manual URL is `GET /pair?code=NNNNNN`, which falls through to the single-page
//! app (the pairing screen); that screen posts the code here. Redeeming is single-use and
//! side-effecting, so it is a POST, never the GET a browser makes when opening the QR link.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_proto::ClockSource;

use crate::pairing::Code;
use crate::state::AppState;

/// A device presenting a pairing code.
#[derive(Debug, Deserialize)]
pub(crate) struct PairRequest {
    /// The six-digit code the operator read from the edge.
    code: String,
}

/// The token issued to a successfully paired device.
#[derive(Debug, Serialize)]
pub(crate) struct PairAccepted {
    /// The opaque bearer token the device presents on later requests.
    device_token: String,
}

/// Redeems a pairing code.
pub(crate) async fn pair(
    State(state): State<AppState>,
    Json(request): Json<PairRequest>,
) -> Response {
    let Some(code) = Code::parse(&request.code) else {
        return (StatusCode::BAD_REQUEST, "a pairing code is six digits").into_response();
    };
    let now = state.clock.now();
    match state.pairing.redeem(&code, now) {
        Ok(Some(token)) => (
            StatusCode::OK,
            Json(PairAccepted {
                device_token: token.as_str().to_owned(),
            }),
        )
            .into_response(),
        // Unknown or expired: the same answer either way, so a probe learns nothing about which.
        Ok(None) => (StatusCode::FORBIDDEN, "unknown or expired pairing code").into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "could not issue a device token",
        )
            .into_response(),
    }
}
