// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The pairing surface: redeem a code, list what is paired, retire one, and mint the next
//! ([ADR-0030](../../../../docs/adr/0030-pairing-and-offline-auth.md),
//! [ADR-0118](../../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md)).
//!
//! The human-facing QR/manual URL is `GET /pair?code=NNNNNN`, which falls through to the single-page
//! app (the pairing screen); that screen posts the code to `POST /api/pair`. Redeeming is
//! single-use and side-effecting, so it is a POST, never the GET a browser makes when opening the
//! QR link.
//!
//! # Two different gates, on purpose
//!
//! `POST /api/pair` is **ungated** — it has to be, because the device presenting a code holds no
//! credential yet — and `GET /api/pair/devices` and `POST /api/pair/revoke` sit behind the
//! paired-device gate. [`mint`] is the one route here behind **both** gates: a paired device *and* a
//! signed-in manager, because minting a credential is a managerial act and the store has a published
//! roster to check that against. See [`codes_router`].

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;

use crate::app::Edge;
use crate::clock::SystemClock;
use crate::pairing::{Code, Pairing, Redeemed};
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
        Ok(Redeemed::Paired(token)) => (
            StatusCode::OK,
            Json(PairAccepted {
                device_token: token.as_str().to_owned(),
            }),
        )
            .into_response(),
        // Unknown or expired: the same answer either way, so a probe learns nothing about which.
        Ok(Redeemed::Rejected) => {
            (StatusCode::FORBIDDEN, "unknown or expired pairing code").into_response()
        }
        // Too many wrong codes (production-readiness S4). `429` with `Retry-After`, so an operator
        // who mistyped a few times is told to wait rather than left guessing why a correct code
        // stopped working — and a script walking the space is told nothing about any code at all.
        Ok(Redeemed::TooManyAttempts { until_ms }) => {
            let seconds = until_ms
                .saturating_sub(now.as_milliseconds_since_epoch())
                .max(0)
                .div_euclid(1_000)
                .saturating_add(1);
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, seconds.to_string())],
                "too many pairing attempts; wait and try again",
            )
                .into_response()
        }
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
    /// Each paired device, newest first — what the operator picks from to retire a lost till.
    paired: Vec<PairedDeviceView>,
}

/// One paired device as the console sees it.
#[derive(Debug, Serialize)]
pub(crate) struct PairedDeviceView {
    /// The device id — what `POST /api/pair/revoke` takes back.
    device_id: String,
    /// When it paired, Unix ms.
    paired_at_ms: i64,
    /// Whether this is the device making the request. The one fact that lets an operator tell their
    /// own tablet from the others, and the row they must **not** retire by accident: doing so signs
    /// this browser out mid-service.
    this_device: bool,
}

/// `GET /api/pair/devices` — which devices are paired, when each paired, and whether that survives a
/// restart.
///
/// The list used to be a bare count, which left an operator with a lost tablet nothing to act on:
/// `POST /api/pair/revoke` takes a device id, and no surface anywhere handed one out
/// (production-readiness **O1**). The pairing instant is the only handle the edge has on *which*
/// tablet a row is — the device's name lives in the cloud's approved-device registry, and a store
/// that has never synced has none — so the console shows when each paired and marks the caller's own
/// row, which together are enough to recognise the odd one out.
///
/// The token digest is never returned: it correlates a device across restarts and buys the console
/// nothing the id does not.
pub(crate) async fn devices(
    State(state): State<AppState>,
    caller: Option<Extension<pos_proto::ids::DeviceId>>,
) -> Response {
    // The gate puts the calling device in the extensions; its absence would be a router-wiring
    // mistake, and marking no row beats marking the wrong one.
    let caller = caller.map(|Extension(device_id)| device_id);
    let paired = state
        .pairing
        .paired_devices()
        .into_iter()
        .map(|(device_id, paired_at)| PairedDeviceView {
            device_id: device_id.to_string(),
            paired_at_ms: paired_at.as_milliseconds_since_epoch(),
            this_device: caller == Some(device_id),
        })
        .collect();
    Json(PairingState {
        devices: state.pairing.issued_count(),
        durable: state.pairing.is_durable(),
        paired,
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

/// What [`mint`] needs: the roster to check standing against, and the pairing state to mint into.
///
/// Its own sub-router state rather than [`AppState`], for the reason `counter` and `print_agent`
/// have theirs: this route needs the *application* [`Edge`] — the published roster lives on its
/// session — and `Edge` is generic over the store, so it cannot ride the erased infrastructure
/// state.
pub(crate) struct PairCodeDeps<S> {
    /// The application edge, read for the published roster only.
    edge: Arc<Edge<S>>,
    /// The pairing state a code is minted into.
    pairing: Arc<Pairing>,
}

impl<S> core::fmt::Debug for PairCodeDeps<S> {
    /// Names its parts and nothing about their contents; [`Pairing`]'s own `Debug` reports counts.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PairCodeDeps")
            .field("pairing", &self.pairing)
            .finish_non_exhaustive()
    }
}

/// `POST /api/pair/codes` — mint the pairing code for the next device.
///
/// Its own sub-router so the caller can layer the **signed-in** gate over it and merge it into the
/// domain router, which layers the **paired-device** gate over everything. Both gates matter and
/// neither is sufficient:
///
/// * paired-only would let any tablet in the shop mint a credential for another, which is the
///   posture `POST /api/pair/revoke` has and is too weak for *issuing*;
/// * signed-in-only is not reachable — the signed-in gate reads the `DeviceId` the paired gate puts
///   in the request extensions, so it cannot run first.
///
/// Deliberately **not** placed beside `/api/pair/devices` on the infrastructure router: that router
/// has no [`Sessions`](crate::auth::Sessions) to layer the second gate with, so a route added there
/// would silently carry a paired-only gate and read, in the router, exactly like one that did not.
pub(crate) fn codes_router<S>(edge: Arc<Edge<S>>, pairing: Arc<Pairing>) -> Router
where
    S: EventStore + Send + Sync + 'static,
{
    Router::new()
        .route("/api/pair/codes", post(mint::<S>))
        .with_state(Arc::new(PairCodeDeps { edge, pairing }))
}

/// The minted code, and when it stops working.
#[derive(Debug, Serialize)]
pub(crate) struct MintedCode {
    /// The six digits the next device presents. **This response body is the only place it exists** —
    /// it is not written to a file and not logged, so a caller that loses this reply mints another.
    code: String,
    /// When the code expires, Unix ms. Returned rather than a duration so a till can show a
    /// countdown without assuming this box's `CODE_TTL`, which is a constant a fork may change.
    expires_at_ms: i64,
}

/// Mints the code for the next device, replacing whatever code was live.
///
/// # Why this route exists
///
/// A code is minted once per process start and nothing else minted another, so adding a second till
/// to a trading store meant restarting the service — which drops every till's and kitchen display's
/// `/ws` session and forces the outbox to drain
/// ([ADR-0117](../../../../docs/adr/0117-a-headless-store-keeps-a-log.md) §70). This removes all but
/// the first of those restarts: the first device on a virgin box still uses the boot code, because
/// it has nothing to authenticate with, and every device after it is minted for from inside the
/// store.
///
/// # What it deliberately does not do
///
/// It writes **no file** and logs **no code**. `POS_EDGE_PAIRING_FILE` is the headless *boot* path
/// and this caller is neither headless nor booting — it is a manager holding a tablet that just
/// received the reply. Minting does unlink that file, because the code it named is now dead
/// ([`Pairing::mint`]).
///
/// The log line carries no employee id either. A durable log records identifiers and counts, and a
/// per-employee managerial-activity stream on a shop-floor box is the thing ADR-0117 §6 kept off
/// disk; who admitted a device is answered by the durable pairing record, not by this line
/// ([ADR-0118](../../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §5).
async fn mint<S>(
    State(deps): State<Arc<PairCodeDeps<S>>>,
    Extension(actor): Extension<Actor>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    if !may_manage_devices(&deps.edge, actor) {
        return needs_manage_devices();
    }
    match deps.pairing.mint(SystemClock.now()) {
        Ok((code, expires_at_ms)) => {
            // No code and no employee id. That a manager minted one is the whole durable fact.
            tracing::info!("a new pairing code was minted for the next device");
            (
                StatusCode::OK,
                Json(MintedCode {
                    code: code.as_str().to_owned(),
                    expires_at_ms,
                }),
            )
                .into_response()
        }
        // The OS entropy source is unavailable. Refuse rather than fake a code, exactly as the
        // boot path does — a predictable pairing code is worse than no pairing code.
        Err(error) => {
            tracing::error!(%error, "could not mint a pairing code: no OS entropy");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "could not mint a pairing code",
            )
                .into_response()
        }
    }
}

/// Whether the signed-in person may admit a device to this store.
///
/// Read from the roster the cloud published, never from anything the request carried: the store
/// authorises against the set the console published and invents nothing (ADR-0070). The same
/// permission ADR-0112 put on binding a print agent, because both are the same act — deciding which
/// hardware this store answers to.
fn may_manage_devices<S>(edge: &Edge<S>, actor: Actor) -> bool {
    edge.session()
        .staff
        .permissions_for(actor.employee_id)
        .is_some_and(|granted| granted.contains(Permission::ManageDevices))
}

/// The refusal a signed-in person without the permission gets.
///
/// `403` rather than `401`: the device is paired and somebody *is* signed in, so sending the till
/// back to the sign-in screen would be the wrong instruction. What is missing is standing.
fn needs_manage_devices() -> Response {
    (
        StatusCode::FORBIDDEN,
        "minting a pairing code needs a manager signed in on this device",
    )
        .into_response()
}
