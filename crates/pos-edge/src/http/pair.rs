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
    match state.pairing.redeem(&code, now).await {
        Ok(Some(token)) => (
            StatusCode::OK,
            Json(PairAccepted {
                device_token: token.as_str().to_owned(),
            }),
        )
            .into_response(),
        // Unknown or expired: the same answer either way, so a probe learns nothing about which.
        Ok(None) => (StatusCode::FORBIDDEN, "unknown or expired pairing code").into_response(),
        // No entropy, or a registry that could not record the device (ADR-0091). Both refuse rather
        // than hand out a token that might not survive the next restart, and both are logged with
        // the cause — the operator needs to know whether the machine or the disk is the problem.
        Err(error) => {
            tracing::error!(error = %error, "refusing to pair a device");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "could not issue a device token",
            )
                .into_response()
        }
    }
}

/// The state of this store's device pairing, for the operator console.
#[derive(Debug, Serialize)]
pub(crate) struct PairingState {
    /// How many devices the store has admitted.
    devices: usize,
    /// Whether those survive a restart (ADR-0091). Reported because an operator planning a reboot
    /// mid-service needs to know whether it will cost them the fleet.
    durable: bool,
}

/// `GET /api/pair/devices` — how many devices are paired, and whether that survives a restart.
///
/// Counts only. Listing device ids would be a reasonable console feature and is not this slice:
/// naming them is only useful alongside *which* tablet each one is, and the edge does not know —
/// the cloud's approved-device registry does.
pub(crate) async fn devices(State(state): State<AppState>) -> Response {
    Json(PairingState {
        devices: state.pairing.issued_count(),
        durable: state.pairing.is_durable(),
    })
    .into_response()
}

/// `POST /api/pair/revoke` — retire one device, or every device.
///
/// Behind the paired-device gate: a device that is itself paired can retire another, which is the
/// posture the pairing surface already has (whoever holds a token stands at the till). Making this
/// an operator-only action needs an operator identity the edge does not have offline — the console
/// is a browser on the LAN, not an authenticated admin — so it is deliberately as strong as pairing
/// and no stronger, and it is recorded in the log.
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Json(request): Json<RevokeRequest>,
) -> Response {
    let outcome = match request.device_id.as_deref() {
        // Every device: the break-glass that reproduces, on purpose, what a restart used to do by
        // accident (ADR-0091).
        None => {
            tracing::warn!("revoking every paired device");
            state.pairing.revoke_all().await
        }
        Some(text) => {
            let Ok(ulid) = text.parse::<pos_proto::ulid::Ulid>() else {
                return (StatusCode::BAD_REQUEST, "device_id is not a ULID").into_response();
            };
            let device_id = pos_proto::ids::DeviceId::new(ulid);
            tracing::warn!(%device_id, "revoking a paired device");
            state.pairing.revoke(device_id).await
        }
    };
    match outcome {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        // The durable table could not be written, so the device may still be paired after a
        // restart. Refuse rather than report success: an operator told a lost tablet is locked out
        // when it is not is worse than one told to try again.
        Err(error) => {
            tracing::error!(error = %error, "could not revoke");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "could not revoke: the device registry is unavailable",
            )
                .into_response()
        }
    }
}

/// Which device to retire. An absent `device_id` means every device.
#[derive(Debug, Deserialize)]
pub(crate) struct RevokeRequest {
    /// The device to retire, or `None` for all of them.
    #[serde(default)]
    device_id: Option<String>,
}
