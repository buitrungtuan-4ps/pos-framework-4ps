// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Edge device activation: the first-boot exchange and the boot gate
//! ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md),
//! [ADR-0053](../../../docs/adr/0053-cloud-sync-port.md)).
//!
//! A fresh box holds no credential, so it cannot trade. The operator presents the activation code
//! printed on its setup sheet; the box exchanges it with the cloud over [`CloudSync`], stores the
//! long-lived credential it gets back in the [`KeyVault`] under
//! [`SecretName::DeviceCredential`], and records `device.activation.completed`. From then on the boot
//! gate ([`boot_standing`]) sees the credential and reports the box [`ActivationStanding::Activated`].
//!
//! # A sub-router with its own state
//!
//! These routes need [`CloudSync`] and [`KeyVault`], which are compile-time-selected ports with no
//! `Dyn` mirror, so they cannot ride the concrete `AppState`. Instead this builds its own generic
//! [`activation_router`], finalised with [`Router::with_state`] and merged into the app — the same
//! shape the cloud's activation routes take. The store `S` rides along only so the completion event
//! can be appended through the [`Edge`] that owns the log.
//!
//! # The vault is the source of truth
//!
//! "Activated" means "a device credential is in the vault", because that is what the boot gate reads.
//! So the credential is stored *before* success is announced, and a second activation attempt on a
//! box that already holds one is a conflict, not a re-exchange — the cloud would refuse the spent
//! code anyway ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use pos_core::activation::{ActivationCode, ActivationStanding, device_activation};
use pos_ports::cloud_sync::CloudSync;
use pos_ports::event_store::EventStore;
use pos_ports::key_vault::{KeyVault, SecretName};
use pos_ports::{PortError, Secret};
use pos_proto::ErrorStatus;

use crate::app::Edge;
use crate::http::ERROR_REASON_HEADER;
use crate::lease_state::LeaseWatch;
use crate::ota_client::RestartRequest;

/// The collaborators the activation routes compose: the [`Edge`] (to append the completion event),
/// the [`CloudSync`] channel (to exchange the code), and the [`KeyVault`] (to store the credential).
///
/// Cheap to clone — every field is behind an [`Arc`] — because axum hands each handler a clone.
struct ActivationState<S, C, V> {
    edge: Arc<Edge<S>>,
    cloud: Arc<C>,
    vault: Arc<V>,
    /// This box's lease, so activation can forget a generation it inherited from a copied
    /// `store.sqlite` ([ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
    /// `None` on a router built without one — the on-fakes example and the tests.
    lease: Option<Arc<LeaseWatch>>,
    /// How a successful activation starts the cloud loops: by asking for the same graceful restart an
    /// installed update asks for. `None` on a router built without one — the tests and a box with no
    /// service manager to start it again.
    restart: Option<Arc<dyn RestartRequest>>,
}

// Hand-written rather than derived, so the state is `Clone` whatever `S`/`C`/`V` are (they sit behind
// `Arc`); a derive would demand `S: Clone` and friends that the ports do not promise.
impl<S, C, V> Clone for ActivationState<S, C, V> {
    fn clone(&self) -> Self {
        Self {
            edge: Arc::clone(&self.edge),
            cloud: Arc::clone(&self.cloud),
            lease: self.lease.clone(),
            vault: Arc::clone(&self.vault),
            restart: self.restart.clone(),
        }
    }
}

/// How long after answering a successful activation the edge asks for its restart — long enough
/// for the answer to leave, short enough that the till's "restarting" message is brief.
const RESTART_AFTER_ACTIVATION: core::time::Duration = core::time::Duration::from_millis(500);

/// A device presenting its activation code.
#[derive(Debug, Deserialize)]
struct ActivateRequest {
    /// The `XXXX-XXXX-XXXX` code the operator typed, in any casing or spacing.
    code: String,
}

/// The identity a successful activation grants.
#[derive(Debug, Serialize)]
struct ActivateAccepted {
    /// The device id the stored credential now authenticates as.
    device_id: String,
}

/// This box's activation standing, for the boot gate and the UI.
#[derive(Debug, Serialize)]
struct StandingResponse {
    /// `true` once a device credential is in the vault.
    activated: bool,
}

/// Builds the activation sub-router: `POST /api/activate` and `GET /api/activation`.
///
/// Finalised with its own state and merged into the app router by the composition layer, so the
/// concrete `AppState` never learns the [`CloudSync`]/[`KeyVault`] types.
pub fn activation_router<S, C, V>(
    edge: Arc<Edge<S>>,
    cloud: Arc<C>,
    vault: Arc<V>,
    lease: Option<Arc<LeaseWatch>>,
    origins: &Arc<crate::origins::Origins>,
    restart: Option<Arc<dyn RestartRequest>>,
) -> Router
where
    S: EventStore + Send + Sync + 'static,
    C: CloudSync + Send + Sync + 'static,
    V: KeyVault + Send + Sync + 'static,
{
    // `GET /api/activation` is covered and `POST /api/activate` is not, so they are layered
    // separately rather than as one router (ADR-0111).
    //
    // The standing route reads as an activation route and belongs with its sibling, but the shipped
    // app disagrees: `App.tsx`'s `onMount` calls it on **every boot**, ahead of pairing and ahead of
    // sign-in, and routes the operator to `/setup` when the box is not activated. It is the first
    // call any front-end makes, and it is wrapped in `.catch(() => routeDevice())` — so leaving it
    // same-origin-only would make a second origin's first request fail *softly*, and an unactivated
    // hosted box would silently never route anyone to `/setup`. It returns a standing boolean.
    //
    // `POST /api/activate` exchanges a code from the store's setup sheet for a long-lived machine
    // credential that lands in the box's OS keyring (ADR-0086). A route that mints a machine
    // credential is not reachable from a page on another origin, and there is no cross-origin actor
    // in that story: an operator activates at the `/setup` screen the box itself serves.
    let state = ActivationState {
        edge,
        cloud,
        vault,
        lease,
        restart,
    };
    Router::new()
        .route("/api/activate", post(activate::<S, C, V>))
        .merge(
            Router::new()
                .route("/api/activation", get(standing::<S, C, V>))
                .layer(crate::origins::cors_layer(origins))
                .with_state(state.clone()),
        )
        .with_state(state)
}

/// This box's activation standing: [`ActivationStanding::Activated`] once a device credential is in
/// the vault, [`ActivationStanding::NeedsActivation`] before then.
///
/// The one boot-time check a composing binary runs to decide whether the box may trade.
///
/// # Errors
///
/// [`PortError`] if the vault itself could not be read — distinct from an absent credential, which is
/// simply [`ActivationStanding::NeedsActivation`].
pub async fn boot_standing<V: KeyVault>(vault: &V) -> Result<ActivationStanding, PortError> {
    let present = vault.load(SecretName::DeviceCredential).await?.is_some();
    Ok(device_activation(present))
}

/// `POST /api/activate` — exchange the code, store the credential, record the completion.
async fn activate<S, C, V>(
    State(state): State<ActivationState<S, C, V>>,
    Json(request): Json<ActivateRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
    C: CloudSync + Send + Sync + 'static,
    V: KeyVault + Send + Sync + 'static,
{
    // A malformed code never named a real one, so refuse it locally: a plain client error, and a
    // saved round-trip. `parse` normalises casing and spacing exactly as the cloud does.
    if ActivationCode::parse(&request.code).is_err() {
        return refused(
            StatusCode::BAD_REQUEST,
            "ACTIVATION_CODE_MALFORMED",
            "the activation code is malformed",
        );
    }

    // Idempotency and no double-spend: a box that already holds a credential is activated. Re-running
    // must not re-exchange, because the cloud would refuse the now-spent code and turn a harmless
    // repeat into a `403`.
    match state.vault.load(SecretName::DeviceCredential).await {
        Ok(Some(_already)) => {
            return refused(
                StatusCode::CONFLICT,
                "ALREADY_ACTIVATED",
                "this device is already activated",
            );
        }
        Ok(None) => {}
        Err(error) => return activation_error(&error),
    }

    let grant = match state.cloud.activate(&request.code).await {
        Ok(grant) => grant,
        Err(error) => return activation_error(&error),
    };

    // Store the credential before announcing success: the vault is what the boot gate reads, so a
    // credential that reached it is what makes the box activated.
    if let Err(error) = store_credential(state.vault.as_ref(), &grant.credential).await {
        return activation_error(&error);
    }

    // A box being activated is a box being provisioned, and this is the one moment that is a fact
    // rather than a guess — so it forgets any lease generation it inherited (ADR-0123). Without
    // this, a replacement built from a copy of the dead box's `store.sqlite` — which
    // `docs/guides/bring-a-store-online.md` tells the operator to make, because it is the only way
    // to recover unpublished events — comes up holding the dead box's generation, reads itself
    // superseded, and refuses to seat a table. After the credential lands, so a failed exchange
    // leaves the box exactly as it was.
    if let Some(lease) = &state.lease {
        lease.adopt_this_box().await;
    }

    // The completion event is a notification to the cloud, not the source of truth (the vault is), so
    // a box that has, in fact, activated is not failed back to the operator because the log write
    // slipped — it is logged and reconciled. `device.activation.completed` still carries the id.
    if let Err(error) = state.edge.record_activation(grant.device_id).await {
        tracing::error!(%error, "activation completed but the completion event could not be recorded");
    }

    // The cloud loops are started at boot, behind the activation gate, so a box activated while it
    // runs would otherwise sit unsynced until somebody restarted it — which the bring-up guide never
    // told anyone to do. A restart is the path every boot already takes: the loops, the OTA client
    // and the stop-drain are all composed exactly as they would be. Asked for a moment after this
    // answer leaves, and drained like any other stop, so the response reaches the till.
    if let Some(restart) = state.restart.clone() {
        tracing::info!("activated: restarting so the cloud loops start with the new credential");
        tokio::spawn(async move {
            tokio::time::sleep(RESTART_AFTER_ACTIVATION).await;
            restart.request_restart();
        });
    }

    (
        StatusCode::OK,
        Json(ActivateAccepted {
            device_id: grant.device_id.to_string(),
        }),
    )
        .into_response()
}

/// Stores the device credential, named so a secret write is conspicuous in a diff.
async fn store_credential<V: KeyVault>(vault: &V, credential: &Secret) -> Result<(), PortError> {
    vault.store(SecretName::DeviceCredential, credential).await
}

/// `GET /api/activation` — report whether this box holds a device credential yet.
async fn standing<S, C, V>(State(state): State<ActivationState<S, C, V>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
    C: CloudSync + Send + Sync + 'static,
    V: KeyVault + Send + Sync + 'static,
{
    match boot_standing(state.vault.as_ref()).await {
        Ok(standing) => (
            StatusCode::OK,
            Json(StandingResponse {
                activated: matches!(standing, ActivationStanding::Activated),
            }),
        )
            .into_response(),
        Err(error) => activation_error(&error),
    }
}

/// Maps a [`PortError`] to the status the activation routes answer with.
///
/// A refusal is `403` with no oracle ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md));
/// a malformed code is `400`; an unreachable cloud or vault is `503`, the operator's cue to retry.
fn activation_error(error: &PortError) -> Response {
    let status = match error.status() {
        ErrorStatus::InvalidArgument => StatusCode::BAD_REQUEST,
        ErrorStatus::PermissionDenied => StatusCode::FORBIDDEN,
        ErrorStatus::Unauthenticated => StatusCode::UNAUTHORIZED,
        ErrorStatus::NotFound => StatusCode::NOT_FOUND,
        ErrorStatus::AlreadyExists | ErrorStatus::FailedPrecondition => StatusCode::CONFLICT,
        // Unreachable on this path — nothing here is a conditional write (ADR-0094), and no
        // `PortError` constructor produces a 422 (ADR-0096) — but named rather than folded into a
        // catch-all, so the next status added to the envelope fails the build here instead of
        // silently becoming a `500`.
        ErrorStatus::VersionMismatch => StatusCode::PRECONDITION_FAILED,
        ErrorStatus::Unprocessable => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorStatus::Unavailable | ErrorStatus::ResourceExhausted => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        ErrorStatus::Internal | ErrorStatus::Unspecified => StatusCode::INTERNAL_SERVER_ERROR,
    };
    // The message is generic on purpose: a refused code must not reveal whether it was spent,
    // revoked, or never real — and neither may its token, which is why every flavour of "no" from
    // the cloud is the one `ACTIVATION_REFUSED`.
    let (reason, body) = match status {
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND => {
            ("ACTIVATION_REFUSED", "activation refused")
        }
        StatusCode::BAD_REQUEST => (
            "ACTIVATION_CODE_MALFORMED",
            "the activation code is malformed",
        ),
        StatusCode::CONFLICT => (
            "ACTIVATION_WRONG_STATE",
            "the device is in the wrong state for activation",
        ),
        StatusCode::SERVICE_UNAVAILABLE => (
            "ACTIVATION_UNAVAILABLE",
            "the activation service is unavailable",
        ),
        _ => ("INTERNAL", "activation failed"),
    };
    refused(status, reason, body)
}

/// A refusal the till can put in the operator's language: the status, the stable reason token the
/// rest of `/api` carries ([ADR-0137](../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)),
/// and the English sentence a log reads. Without the token, `/setup` could only show that sentence
/// — English, on the one screen a technician meets before anything else works.
fn refused(status: StatusCode, reason: &'static str, body: &'static str) -> Response {
    (
        status,
        [(ERROR_REASON_HEADER, HeaderValue::from_static(reason))],
        body,
    )
        .into_response()
}
