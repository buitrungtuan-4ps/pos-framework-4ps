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
use axum::http::StatusCode;
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

/// The collaborators the activation routes compose: the [`Edge`] (to append the completion event),
/// the [`CloudSync`] channel (to exchange the code), and the [`KeyVault`] (to store the credential).
///
/// Cheap to clone — every field is behind an [`Arc`] — because axum hands each handler a clone.
struct ActivationState<S, C, V> {
    edge: Arc<Edge<S>>,
    cloud: Arc<C>,
    vault: Arc<V>,
}

// Hand-written rather than derived, so the state is `Clone` whatever `S`/`C`/`V` are (they sit behind
// `Arc`); a derive would demand `S: Clone` and friends that the ports do not promise.
impl<S, C, V> Clone for ActivationState<S, C, V> {
    fn clone(&self) -> Self {
        Self {
            edge: Arc::clone(&self.edge),
            cloud: Arc::clone(&self.cloud),
            vault: Arc::clone(&self.vault),
        }
    }
}

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
    origins: &Arc<crate::origins::Origins>,
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
    let state = ActivationState { edge, cloud, vault };
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
        return (StatusCode::BAD_REQUEST, "the activation code is malformed").into_response();
    }

    // Idempotency and no double-spend: a box that already holds a credential is activated. Re-running
    // must not re-exchange, because the cloud would refuse the now-spent code and turn a harmless
    // repeat into a `403`.
    match state.vault.load(SecretName::DeviceCredential).await {
        Ok(Some(_already)) => {
            return (StatusCode::CONFLICT, "this device is already activated").into_response();
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

    // The completion event is a notification to the cloud, not the source of truth (the vault is), so
    // a box that has, in fact, activated is not failed back to the operator because the log write
    // slipped — it is logged and reconciled. `device.activation.completed` still carries the id.
    if let Err(error) = state.edge.record_activation(grant.device_id).await {
        tracing::error!(%error, "activation completed but the completion event could not be recorded");
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
    // revoked, or never real.
    let body = match status {
        StatusCode::FORBIDDEN => "activation refused",
        StatusCode::BAD_REQUEST => "the activation code is malformed",
        StatusCode::CONFLICT => "the device is in the wrong state for activation",
        StatusCode::SERVICE_UNAVAILABLE => "the activation service is unavailable",
        _ => "activation failed",
    };
    (status, body).into_response()
}
