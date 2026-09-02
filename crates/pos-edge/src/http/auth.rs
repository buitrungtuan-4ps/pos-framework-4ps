// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Device authentication and staff sign-in on the domain routes
//! ([ADR-0084](../../../docs/adr/0084-device-authentication.md)).
//!
//! Two gates guard the domain surface, in order:
//!
//! 1. [`require_paired_device`] — every request must present the bearer token a device was issued when
//!    it paired ([`crate::pairing`]). This is what closes "any host on the store LAN commands the
//!    edge": an unpaired tablet — or a laptop plugged into the store switch — is refused `401` before
//!    it reaches a handler. [`require_paired_device_ws`] is the same gate for `/ws`, which needs a
//!    second way to present the token because a browser cannot set a header on a WebSocket.
//! 2. [`require_signed_in`] — every *command* (and the store reads beside them) additionally requires a
//!    real employee signed in on that device. The command then runs under that person's
//!    [`Actor`], not a placeholder — so every sale, void and shift is attributable to who did it
//!    (S0b). The **session routes** below ([`sign_in`], [`sign_out`], [`current`]) sit behind the first
//!    gate but not the second: signing in is how a device passes the second gate, so it cannot itself
//!    require having passed it.
//!
//! Both gates run as middleware rather than per-handler extractors, so they guard the whole router in
//! one place — reads included: an unpaired or unsigned device has no more business reading the store's
//! tables than commanding them. On success the resolved [`DeviceId`] (first gate) and [`Actor`] (second
//! gate) ride in the request's extensions, and the command handlers read the `Actor`.
//!
//! # No secret in a log
//!
//! A PIN never enters a log, a span, or a response. A sign-in logs only the employee id (an identifier,
//! not PII) and the outcome — see [`crate::telemetry`].

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;
use pos_proto::ids::DeviceId;

use crate::app::Edge;
use crate::auth::{Lockout, Sessions, SignIn};
use crate::clock::SystemClock;
use crate::pairing::{DeviceToken, Pairing};

/// Refuses a request that does not carry a valid device token, and resolves the paired device for the
/// ones that do — placing its [`DeviceId`] in the request extensions for the next gate and the handler.
///
/// Absent, malformed, and unknown tokens all get the same `401`, so a probe learns nothing about which
/// of the three it hit.
pub(crate) async fn require_paired_device(
    State(pairing): State<Arc<Pairing>>,
    request: Request,
    next: Next,
) -> Response {
    gate(&pairing, bearer(&request), request, next).await
}

/// The same gate for `GET /ws`, which accepts the token from **either** the `Authorization` header or
/// the WebSocket subprotocol list.
///
/// # Why `/ws` needs a second channel
///
/// The store UI is a browser, and the browser `WebSocket` API cannot set request headers — there is
/// no way to send `Authorization` on an upgrade. So the token also travels in
/// `Sec-WebSocket-Protocol`, which the API *can* set (`new WebSocket(url, protocols)`).
///
/// The alternative was a query parameter, and it was rejected: the edge logs the request path on
/// every request ([`crate::telemetry`]), so `/ws?token=…` would write a device credential into the
/// log — against this module's own "no secret in a log" rule, and one careless log line away from
/// being permanent. A header is not logged.
///
/// `Authorization` is still accepted and tried first, because a non-browser consumer (the
/// third-party KDS the roadmap defers) can set it and should not have to learn this workaround.
pub(crate) async fn require_paired_device_ws(
    State(pairing): State<Arc<Pairing>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = bearer(&request).or_else(|| subprotocol_token(&request));
    gate(&pairing, presented, request, next).await
}

/// Resolves `presented` against the issued tokens, or refuses. Shared by both gates so there is one
/// place that decides what a valid device token buys and what an invalid one is told.
async fn gate(
    pairing: &Pairing,
    presented: Option<DeviceToken>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(device_id) = presented.and_then(|token| pairing.device_for(&token)) else {
        return (
            StatusCode::UNAUTHORIZED,
            "pair this device to reach the edge",
        )
            .into_response();
    };
    request.extensions_mut().insert(device_id);
    next.run(request).await
}

/// Refuses a request from a paired device that has no employee signed in, and resolves the signed-in
/// [`Actor`] for the ones that do — placing it in the request extensions for the handler (S0b).
///
/// Runs after [`require_paired_device`], so the [`DeviceId`] is already in the extensions; its absence
/// would be a router-wiring mistake, and this refuses safely rather than trusting an unauthenticated
/// request. A paired-but-unsigned device is refused **`403`**, distinct from the unpaired **`401`** the
/// first gate returns: the device's credential is valid, it just carries no authorised person yet, so
/// the UI shows the sign-in screen rather than sending the operator back to pair.
pub(crate) async fn require_signed_in(
    State(sessions): State<Arc<Sessions>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(device_id) = request.extensions().get::<DeviceId>().copied() else {
        return (
            StatusCode::UNAUTHORIZED,
            "pair this device to reach the edge",
        )
            .into_response();
    };
    let now = SystemClock.now();
    let Some(employee_id) = sessions.employee_for(device_id, now) else {
        // Nobody signed in, or the device has sat idle past the window (ADR-0091). The same `403`
        // either way, so the UI shows the sign-in screen without having to distinguish them.
        return (StatusCode::FORBIDDEN, "sign in to act on this device").into_response();
    };
    // The device is in use, so it is not idle. Memory always; the durable value is flushed only
    // when it has fallen a minute behind, so this gate does not put a write on every request.
    if sessions.touch(device_id, now)
        && let Err(error) = sessions.flush_last_seen(device_id, now).await
    {
        // Not fatal: the in-memory instant has already moved, so the device keeps working and the
        // only cost is a slightly stale row if the box restarts in the next minute.
        tracing::warn!(error = %error, "could not flush a device's last-seen instant");
    }
    request.extensions_mut().insert(Actor {
        employee_id,
        device_id,
    });
    next.run(request).await
}

/// The device token from an `Authorization: Bearer <token>` header, if present and well-formed.
fn bearer(request: &Request) -> Option<DeviceToken> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| DeviceToken::parse(token.trim()))
}

/// The device token offered as a WebSocket subprotocol, if one of the offered values is a token.
///
/// A client offers a list — `Sec-WebSocket-Protocol: pos-edge.v1, <token>` — and the server echoes
/// only the *name* ([`crate::http::ws::SUBPROTOCOL`]), never the token, so the credential does not
/// come back in the handshake response. Picking the entry out by shape is unambiguous:
/// [`DeviceToken::parse`] accepts exactly 32 lowercase hex characters, which no protocol name is.
fn subprotocol_token(request: &Request) -> Option<DeviceToken> {
    request
        .headers()
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .find_map(|offered| DeviceToken::parse(offered.trim()))
}

/// What the session routes ([`sign_in`], [`sign_out`], [`current`]) share: the [`Edge`] whose synced
/// roster a sign-in verifies against, the [`Sessions`] map it records the binding in, and the
/// [`Lockout`] that rate-limits PIN attempts. Cheap to clone — every field is an [`Arc`].
pub(crate) struct SignInDeps<S> {
    pub edge: Arc<Edge<S>>,
    pub sessions: Arc<Sessions>,
    pub lockout: Arc<Lockout>,
}

// A manual `Clone` (which axum's `State` requires) so it does not demand `S: Clone` the derive would.
impl<S> Clone for SignInDeps<S> {
    fn clone(&self) -> Self {
        Self {
            edge: Arc::clone(&self.edge),
            sessions: Arc::clone(&self.sessions),
            lockout: Arc::clone(&self.lockout),
        }
    }
}

/// A sign-in: the badge `code` a person types and their `pin`. Both are secrets and never logged.
#[derive(Debug, Deserialize)]
pub(crate) struct SignInRequest {
    code: String,
    pin: String,
}

/// A successful sign-in: the employee the device now acts as.
#[derive(Debug, Serialize)]
pub(crate) struct SignedIn {
    employee_id: String,
}

/// A refused sign-in. `outcome` is `"wrong"` (bad code or PIN) or `"locked_out"`; `remaining` counts
/// attempts left before a lockout, and `locked_until_ms` says when a lockout lifts. Neither the code
/// nor whether it exists is revealed — a bad code and a bad PIN answer the same way.
#[derive(Debug, Serialize)]
pub(crate) struct SignInRefused {
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    remaining: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    locked_until_ms: Option<i64>,
}

/// The current sign-in on a device, for the UI to restore its state after a reload.
#[derive(Debug, Serialize)]
pub(crate) struct SessionState {
    signed_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    employee_id: Option<String>,
}

/// `POST /api/session/sign-in` — verify a badge code + PIN against the synced roster and, on success,
/// bind the signed-in employee to this device (S0b/ADR-0084).
pub(crate) async fn sign_in<S>(
    State(deps): State<SignInDeps<S>>,
    Extension(device_id): Extension<DeviceId>,
    Json(request): Json<SignInRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let now = SystemClock.now();
    let session = deps.edge.session();
    // An unknown code, a member with no id, or one with no PIN set: refuse the same way a wrong PIN is
    // refused, and without running the lockout (there is no employee to key it on), so a probe cannot
    // tell an existing code from a missing one.
    let Some((employee_id, phc)) = session.staff.credentials(&request.code) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(SignInRefused {
                outcome: "wrong",
                remaining: None,
                locked_until_ms: None,
            }),
        )
            .into_response();
    };
    match deps
        .lockout
        .authenticate(employee_id, phc, &request.pin, now)
    {
        SignIn::Ok => {
            // Recorded durably before it is reported (ADR-0091): telling someone they are signed in
            // and then silently forgetting would surface mid-sale.
            if let Err(error) = deps.sessions.sign_in(device_id, employee_id, now).await {
                tracing::error!(error = %error, %employee_id, "could not record a sign-in");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "could not record the sign-in",
                )
                    .into_response();
            }
            tracing::info!(%employee_id, "staff signed in");
            (
                StatusCode::OK,
                Json(SignedIn {
                    employee_id: employee_id.to_string(),
                }),
            )
                .into_response()
        }
        SignIn::Wrong { remaining } => {
            tracing::warn!(%employee_id, remaining, "staff sign-in refused: wrong pin");
            (
                StatusCode::UNAUTHORIZED,
                Json(SignInRefused {
                    outcome: "wrong",
                    remaining: Some(remaining),
                    locked_until_ms: None,
                }),
            )
                .into_response()
        }
        SignIn::LockedOut { until_ms } => {
            tracing::warn!(%employee_id, "staff sign-in refused: locked out");
            (
                StatusCode::UNAUTHORIZED,
                Json(SignInRefused {
                    outcome: "locked_out",
                    remaining: None,
                    locked_until_ms: Some(until_ms),
                }),
            )
                .into_response()
        }
    }
}

/// `POST /api/session/sign-out` — clear the sign-in on this device (end of shift, or handing the tablet
/// on). Idempotent: signing out a device nobody is signed in on still returns `204`.
pub(crate) async fn sign_out<S>(
    State(deps): State<SignInDeps<S>>,
    Extension(device_id): Extension<DeviceId>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    // Memory is cleared first and unconditionally (see `Sessions::sign_out`), so a registry that
    // cannot be written still leaves the device signed *out* on this box. Reporting the failure
    // matters — the durable row will still say signed-in after a restart — but refusing the
    // sign-out would be worse: the operator would believe the till is locked when it is not.
    if let Err(error) = deps.sessions.sign_out(device_id).await {
        tracing::error!(error = %error, "signed out locally but could not clear the durable record");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /api/session` — who is signed in on this (paired) device, so the UI resumes on the right screen
/// after a reload without re-typing a PIN.
pub(crate) async fn current<S>(
    State(deps): State<SignInDeps<S>>,
    Extension(device_id): Extension<DeviceId>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let state = match deps.sessions.employee_for(device_id, SystemClock.now()) {
        Some(employee_id) => SessionState {
            signed_in: true,
            employee_id: Some(employee_id.to_string()),
        },
        None => SessionState {
            signed_in: false,
            employee_id: None,
        },
    };
    Json(state).into_response()
}
