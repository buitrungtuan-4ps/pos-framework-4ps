// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/activation-codes` and `/activate`: the one-time code a new box exchanges for its
//! credential (ADR-0050).
//!
//! A cloud that names its cell issues each code with that cell's id as a prefix,
//! `VN1-XXXX-XXXX-XXXX`, and a code presented with a prefix naming another cell is refused with
//! the directory's answer for where that cell is
//! ([ADR-0181](../../../../docs/adr/0181-the-activation-code-names-its-cell.md)). The prefix is a
//! routing hint and never part of the credential: a code is looked up and spent by its twelve
//! symbols alone.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_api::console::{mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_core::activation::{ActivationCode, Redemption, redeem};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{DeviceId, StoreId, TenantId};
use serde::Deserialize;

use crate::activation::{ActivationCodeStore, hash_code, mint_device_credential};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use pos_cloud_api::console::random_hex_32;

// --- Device activation (`/admin/activation-codes` + `/activate`) --------------------------------

/// The collaborators the activation routes need, stated independently of [`CloudApp`]: the activation
/// store, plus the admin and clock stores the issue/revoke routes' session guard uses, and which cell
/// this is.
#[derive(Clone)]
struct ActivationState<X, A, C> {
    activations: X,
    admin: A,
    clock: C,
    cell: Arc<dyn ActivationCell>,
}

/// Which cell the activation routes answer for
/// ([ADR-0181](../../../../docs/adr/0181-the-activation-code-names-its-cell.md)): the prefix a code is
/// issued with, and where a prefix that names another cell points.
pub trait ActivationCell: Send + Sync {
    /// The cell id codes are issued under: this cell's, once the cloud names its cell, and `None` for
    /// a single cell that issues codes with no prefix.
    fn issuing_cell_id(&self) -> Option<String>;
    /// Whether `cell_id`, a lower-case cell id read from a code's prefix, names this cell.
    fn is_this_cell(&self, cell_id: &str) -> bool;
    /// The base URL the directory this cell holds gives for `cell_id`, when it lists that cell.
    fn cell_base_url(&self, cell_id: &str) -> Option<String>;
}

/// A cloud that knows nothing of cells: it issues no prefix, and takes any prefix for its own.
struct NoCell;

impl ActivationCell for NoCell {
    fn issuing_cell_id(&self) -> Option<String> {
        None
    }

    fn is_this_cell(&self, _cell_id: &str) -> bool {
        true
    }

    fn cell_base_url(&self, _cell_id: &str) -> Option<String> {
        None
    }
}

/// Builds the activation sub-router ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
///
/// A super-admin issues a code bound to a device slot and can cancel a slot's pending code, both
/// behind the `/admin` session guard. A device presents its code on `/activate` and receives its
/// credential — that route is **not** authenticated, because the code itself is the bearer credential
/// (single-use, 55-bit); the exchange is where a box first earns a credential. Like [`device_router`](crate::routes::device_router),
/// it carries its own state and is merged into the main router rather than adding a `CloudApp` generic.
pub fn activation_router<X, A, C>(activations: X, admin: A, clock: C) -> Router
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    activation_router_in_cell(activations, admin, clock, Arc::new(NoCell))
}

/// [`activation_router`] for a cloud that knows which cell it is
/// ([ADR-0181](../../../../docs/adr/0181-the-activation-code-names-its-cell.md)): codes are issued
/// with the cell's prefix, and a refused code whose prefix names another cell says where the
/// directory places that cell.
pub fn activation_router_in_cell<X, A, C>(
    activations: X,
    admin: A,
    clock: C,
    cell: Arc<dyn ActivationCell>,
) -> Router
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/activation-codes",
            post(admin_issue_activation_code::<X, A, C>),
        )
        .route(
            "/admin/activation-codes/revoke",
            post(admin_revoke_activation_codes::<X, A, C>),
        )
        .route("/activate", post(exchange_activation_code::<X, A, C>))
        .with_state(ActivationState {
            activations,
            admin,
            clock,
            cell,
        })
}

/// A super-admin issues an activation code for a device slot.
#[expect(
    clippy::struct_field_names,
    reason = "tenant_id/store_id/device_id are the wire field names; the shared _id postfix is the ULID naming convention (docs/naming-and-api.md), not a smell"
)]
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct IssueActivationRequest {
    /// The tenant the device belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store the device belongs to (a ULID).
    store_id: String,
    /// The device slot to activate (a ULID).
    device_id: String,
}

/// The freshly-issued code, shown once for the operator to put on the setup sheet.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct IssueActivationResponse {
    /// The code for the setup sheet: `XXXX-XXXX-XXXX`, or `VN1-XXXX-XXXX-XXXX` with this cell's id
    /// as its prefix once the cloud names its cell. The only time it is visible; only the hash of its
    /// twelve symbols is stored.
    activation_code: String,
}

/// A super-admin cancels the pending activation for a device slot (a leaked setup sheet).
#[expect(
    clippy::struct_field_names,
    reason = "tenant_id/store_id/device_id are the wire field names; the shared _id postfix is the ULID naming convention (docs/naming-and-api.md), not a smell"
)]
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct RevokeActivationRequest {
    /// The tenant the device belongs to (a ULID).
    tenant_id: String,
    /// The store the device belongs to (a ULID).
    store_id: String,
    /// The device slot whose issued codes to cancel (a ULID).
    device_id: String,
}

/// How many still-issued codes were cancelled.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct RevokeActivationResponse {
    /// The number of codes moved from `issued` to `revoked`.
    revoked: u64,
}

/// A device presents its activation code. Unauthenticated: the code is the credential.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct ExchangeRequest {
    /// The `XXXX-XXXX-XXXX` code the operator typed, in any casing or spacing, with or without its
    /// cell's prefix (`VN1-XXXX-XXXX-XXXX`, ADR-0181). Only its twelve symbols are looked up.
    code: String,
    /// The store the box was installed for, from its `config.toml`. A code issued for another store
    /// is refused before it is spent. Optional, because an edge older than the field sends none.
    #[serde(default)]
    store_id: Option<String>,
}

/// The device's minted credential and the device id it was activated as — shown once.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct ExchangeResponse {
    /// The device id the credential authenticates as.
    device_id: String,
    /// The `posdev_<id>_<secret>` bearer token the device stores in its `KeyVault`.
    credential: String,
}

#[utoipa::path(
    post,
    path = "/admin/activation-codes",
    extensions(("x-permission" = json!("console.devices.manage"))),
    security(("session_cookie" = [])),
    request_body(
        content = IssueActivationRequest, description = "The device slot: `tenant_id`, `store_id` and `device_id`, the device \
                       registered under `/admin/stores/{store_id}/devices`",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = IssueActivationResponse, description = "Issued. The body carries `activation_code`, `XXXX-XXXX-XXXX`, \
                                      for the setup sheet, prefixed with this cell's id \
                                      (`VN1-XXXX-XXXX-XXXX`) once a `[cell]` section names it \
                                      (ADR-0181). It is returned exactly once, since only its hash \
                                      is stored, and it is spent the first time a box exchanges it \
                                      for a credential (ADR-0050)"),
        (status = 400, description = "tenant_id, store_id or device_id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The activation store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "box activation",
)]
/// A super-admin issues an activation code bound to a device slot.
async fn admin_issue_activation_code<X, A, C>(
    State(state): State<ActivationState<X, A, C>>,
    headers: HeaderMap,
    Json(request): Json<IssueActivationRequest>,
) -> Response
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, store_id, device_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("device_id", &request.device_id),
    ]) {
        Ok([tenant_id, store_id, device_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            DeviceId::new(device_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(code) = mint_activation_code() else {
        tracing::error!("could not read OS entropy to mint an activation code");
        return service_unavailable("activation");
    };
    let code = match state.cell.issuing_cell_id() {
        None => code,
        // The cell id was checked against the same rule at boot, so this keeps the prefix; were
        // it ever refused, the code is still issued, without one, and redeems the same.
        Some(cell_id) => code.clone().with_cell_id(&cell_id).unwrap_or_else(|error| {
            tracing::error!(%error, "this cell's id cannot prefix an activation code");
            code
        }),
    };
    match state
        .activations
        .issue(hash_code(&code), tenant_id, store_id, device_id)
        .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(IssueActivationResponse {
                activation_code: code.to_setup_sheet_text(),
            }),
        )
            .into_response(),
        Err(error) => activation_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/activation-codes/revoke",
    extensions(("x-permission" = json!("console.devices.manage"))),
    security(("session_cookie" = [])),
    request_body(
        content = RevokeActivationRequest, description = "The device slot: `tenant_id`, `store_id` and `device_id`, the device \
                       registered under `/admin/stores/{store_id}/devices`",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = RevokeActivationResponse, description = "Every code issued for the slot and not yet spent is cancelled, \
                                      as for a setup sheet that went astray. The body carries \
                                      `revoked`, how many were"),
        (status = 400, description = "tenant_id, store_id or device_id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The activation store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "box activation",
)]
/// A super-admin cancels a device slot's still-issued codes.
async fn admin_revoke_activation_codes<X, A, C>(
    State(state): State<ActivationState<X, A, C>>,
    headers: HeaderMap,
    Json(request): Json<RevokeActivationRequest>,
) -> Response
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, store_id, device_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("device_id", &request.device_id),
    ]) {
        Ok([tenant_id, store_id, device_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            DeviceId::new(device_id),
        ),
        Err(refusal) => return refusal,
    };
    match state
        .activations
        .revoke_slot(tenant_id, store_id, device_id)
        .await
    {
        Ok(revoked) => (StatusCode::OK, Json(RevokeActivationResponse { revoked })).into_response(),
        Err(error) => activation_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/activate",
    security(),
    extensions(("x-public-reason" = json!("a single-use activation code from the store's setup sheet"))),
    request_body(
        content = ExchangeRequest, description = "`code`, the activation code from the store's setup sheet, and optionally the \
                       `store_id` the box expects to belong to",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = ExchangeResponse, description = "Activated: the `device_id`, and the device `credential`, returned this \
                                      once; the cloud keeps only its hash (ADR-0050)"),
        (status = 400, description = "The code or the store id is malformed", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "Refused: a spent, revoked, unknown or raced code, or one issued for \
                                      another store, all answer the same. A code whose cell prefix \
                                      names another cell says so too: a `code` detail \
                                      `OTHER_CELL`, with the directory's base URL for that cell in \
                                      the message, or `UNKNOWN_CELL` when the directory does not \
                                      list it (ADR-0181)", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many activation attempts from this client: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The activation service, or the OS entropy source, is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "box activation",
)]
/// A device exchanges its activation code for a credential (unauthenticated — the code is the
/// credential). Single-use and deny-by-default, per [`pos_core::activation::redeem`].
async fn exchange_activation_code<X, A, C>(
    State(state): State<ActivationState<X, A, C>>,
    Json(request): Json<ExchangeRequest>,
) -> Response
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let Ok(code) = ActivationCode::parse(&request.code) else {
        // A malformed code never named a real one, so this is a plain client error, not an oracle.
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the activation code is malformed",
            &[("code", "MALFORMED")],
        );
    };
    let claimed_store = match request.store_id.as_deref().map(str::parse::<StoreId>) {
        None => None,
        Some(Ok(store_id)) => Some(store_id),
        // Says nothing about the code, which has not been looked at yet.
        Some(Err(_)) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "the store id is malformed",
                &[("store_id", "MALFORMED")],
            );
        }
    };
    let code_hash = hash_code(&code);
    let issued = match state.activations.lookup(code_hash).await {
        Ok(Some(issued)) => issued,
        // An unknown code collapses to the same refusal a spent one gets — no oracle.
        Ok(None) => return activation_refused_in(&code, state.cell.as_ref()),
        Err(error) => return activation_error_response(&error),
    };
    // A code for another store is refused **unspent**, so the technician who typed the wrong shop's
    // code can still use it on the right box. The same refusal as an unknown code: whoever holds a
    // valid code learns nothing from naming a store that they could not learn by leaving it out.
    if claimed_store.is_some_and(|store_id| store_id != issued.store_id) {
        tracing::info!("activation refused: the code was issued for another store");
        return activation_refused_in(&code, state.cell.as_ref());
    }
    match redeem(issued.status) {
        Redemption::Reject(reason) => {
            // The reason is for the server's log; the device sees one generic refusal.
            tracing::info!(?reason, "activation refused");
            activation_refused_in(&code, state.cell.as_ref())
        }
        Redemption::Grant => {
            let now_ms = state.clock.now().as_milliseconds_since_epoch();
            let (Some(id), Some(secret)) = (mint_ulid(now_ms), random_hex_32()) else {
                tracing::error!("could not read OS entropy to mint a device credential");
                return service_unavailable("activation");
            };
            let (credential, token) = mint_device_credential(id, &secret);
            match state
                .activations
                .consume_and_provision(code_hash, &credential)
                .await
            {
                Ok(true) => (
                    StatusCode::CREATED,
                    Json(ExchangeResponse {
                        device_id: issued.device_id.to_string(),
                        credential: token,
                    }),
                )
                    .into_response(),
                // The code was spent or revoked between the lookup and the consume — same refusal.
                Ok(false) => activation_refused_in(&code, state.cell.as_ref()),
                Err(error) => activation_error_response(&error),
            }
        }
    }
}

/// The one generic activation refusal. A spent, revoked, unknown, or raced code all collapse to this,
/// so a prober cannot tell them apart ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
fn activation_refused() -> Response {
    api_error(ErrorStatus::PermissionDenied, "activation refused")
}

/// [`activation_refused`], plus the directory's answer when the code's prefix names another cell
/// ([ADR-0181](../../../../docs/adr/0181-the-activation-code-names-its-cell.md)).
///
/// It adds the same answer to every refusal of such a code, whatever the reason, so it says nothing
/// of the code that the bare refusal does not: only where the cell the caller named is.
fn activation_refused_in(code: &ActivationCode, cell: &dyn ActivationCell) -> Response {
    let Some(cell_id) = code.cell_id().filter(|cell_id| !cell.is_this_cell(cell_id)) else {
        return activation_refused();
    };
    match cell.cell_base_url(cell_id) {
        Some(base_url) => api_error_with_details(
            ErrorStatus::PermissionDenied,
            format!(
                "activation refused: the code names the cell {cell_id}, which the cell directory \
                 places at {base_url}; present it there"
            ),
            &[("code", "OTHER_CELL")],
        ),
        None => api_error_with_details(
            ErrorStatus::PermissionDenied,
            format!(
                "activation refused: the code names the cell {cell_id}, which is not this cell and \
                 which the cell directory here does not list"
            ),
            &[("code", "UNKNOWN_CELL")],
        ),
    }
}

/// Maps an activation-store failure to a retryable `503`, logging the detail rather than leaking it.
fn activation_error_response(error: &crate::activation::ActivationStoreError) -> Response {
    tracing::error!(%error, "an activation store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the activation service is unavailable",
    )
}

/// Mints a fresh activation code from OS entropy, or `None` if the entropy source is unavailable — in
/// which case the caller fails closed rather than issue a guessable code.
fn mint_activation_code() -> Option<ActivationCode> {
    let mut entropy = [0_u8; pos_core::activation::PAYLOAD_LEN];
    getrandom::fill(&mut entropy).ok()?;
    Some(ActivationCode::from_entropy(entropy))
}
