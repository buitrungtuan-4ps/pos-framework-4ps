// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store proposes the devices it discovers; an operator lists, approves or rejects them.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::devices::DeviceConnection;
use pos_proto::ids::{StationId, StoreId, TenantId};

use super::{
    DeviceState, DeviceTenantQuery, ProposeDeviceRequest, ProposeDeviceResponse,
    device_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::auth::console_rbac::ConsolePermission;
use crate::devices::{
    DeviceKind, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore, DeviceProposalSummary,
    PersistedDeviceProposal,
};

#[utoipa::path(
    post,
    path = "/sync/stores/{store_id}/devices",
    security(("device_credential" = ["manage_devices"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
    ),
    request_body(
        content = ProposeDeviceRequest, description = "A device the store discovered: `kind` (`printer` or `kds`), `name` and `address` \
                       (ADR-0041)",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = ProposeDeviceResponse, description = "Proposed: its `id`, and `status` pending until an operator approves it \
                                      at the console"),
        (status = 400, description = "store_id is not a ULID, kind is not a known device kind, or name or \
                                      address is blank", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `manage_devices` scope, or belongs to another \
                                      store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many requests to the store sync surface: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store, or the device service, is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "devices",
)]
/// A store proposes a discovered device (`manage_devices` scope). Stored `pending` for an operator to
/// approve; the id is minted here and returned once.
pub(super) async fn propose_device<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<ProposeDeviceRequest>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ManageDevices) {
        return forbidden.into_response();
    }
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let Some(kind) = DeviceKind::from_wire(&request.kind) else {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "kind must be one of printer, kds",
            &[("kind", "UNKNOWN_VALUE")],
        );
    };
    // Reported per field rather than as one "name and address are required", because a caller that
    // sent a name and forgot an address should be told which of the two to fix. The condition is
    // unchanged; only the answer got more specific.
    if request.name.trim().is_empty() || request.address.trim().is_empty() {
        let mut missing: Vec<(&str, &str)> = Vec::with_capacity(2);
        if request.name.trim().is_empty() {
            missing.push(("name", "REQUIRED"));
        }
        if request.address.trim().is_empty() {
            missing.push(("address", "REQUIRED"));
        }
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name and address are required",
            &missing,
        );
    }
    let Some(id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DeviceProposalId::new)
    else {
        tracing::error!("could not read OS entropy to mint a device-proposal id");
        return service_unavailable("device");
    };
    let proposal = PersistedDeviceProposal {
        id,
        tenant_id: grant.tenant(),
        store_id,
        kind,
        name: request.name,
        address: request.address,
    };
    match state.devices.propose(&proposal).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(ProposeDeviceResponse {
                id: id.to_string(),
                status: DeviceProposalStatus::Pending.as_wire().to_owned(),
            }),
        )
            .into_response(),
        Err(error) => device_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/sync/stores/{store_id}/devices",
    security(("device_credential" = ["manage_devices"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
    ),
    responses(
        (status = 200, body = [DeviceProposalSummary], description = "The store's approved devices, never a raw discovery"),
        (status = 400, description = "store_id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `manage_devices` scope, or belongs to another \
                                      store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many requests to the store sync surface: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store, or the device service, is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "devices",
)]
/// A store lists its **approved** devices (`manage_devices` scope) — what the edge acts on, never a
/// raw discovery.
pub(super) async fn list_store_devices<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ManageDevices) {
        return forbidden.into_response();
    }
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    match state
        .devices
        .list(
            grant.tenant(),
            Some(store_id),
            DeviceProposalStatus::Approved,
        )
        .await
    {
        Ok(devices) => {
            (StatusCode::OK, Json::<Vec<DeviceProposalSummary>>(devices)).into_response()
        }
        Err(error) => device_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/devices/proposals",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose devices to list (a 26-character ULID)"),
        ("store_id" = Option<String>, Query, description = "Only this store's devices (a 26-character \
                                                          ULID). Absent, the tenant's whole fleet"),
        ("status" = Option<String>, Query, description = "`pending` (the default: the approval \
                                                        queue), `approved` or `rejected`"),
    ),
    responses(
        (status = 200, body = [DeviceProposalSummary], description = "The devices, as an array: each with `id`, `store_id`, `kind`, \
                                      `name`, `address`, `status` and `version`, and once approved \
                                      its `connection`, `station_id`, print agent, drawer mark, \
                                      paper, and a till's receipt printer, receipt languages and \
                                      opening float, where set"),
        (status = 400, description = "tenant_id is absent or not a ULID, store_id is not a ULID, or \
                                      status is not one of the three", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The device store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "device onboarding",
)]
/// A super-admin lists a tenant's pending device proposals — the approval queue.
pub(super) async fn list_pending_devices<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Query(query): Query<DeviceTenantQuery>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let store_id = match query.store_id.as_deref() {
        None => None,
        Some(raw) => match parse_ulid_fields([("store_id", raw)]) {
            Ok([store_id]) => Some(StoreId::new(store_id)),
            Err(refusal) => return refusal,
        },
    };
    // A closed set, refused by name rather than silently narrowed: a caller who mistypes `aproved`
    // and is handed the pending queue would read it as "this store has no printers".
    let status = match query.status.as_deref() {
        None | Some("pending") => DeviceProposalStatus::Pending,
        Some("approved") => DeviceProposalStatus::Approved,
        Some("rejected") => DeviceProposalStatus::Rejected,
        Some(_) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "status must be one of: pending, approved, rejected",
                &[("status", "INVALID_VALUE")],
            );
        }
    };
    match state.devices.list(tenant_id, store_id, status).await {
        Ok(devices) => {
            (StatusCode::OK, Json::<Vec<DeviceProposalSummary>>(devices)).into_response()
        }
        Err(error) => device_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/devices/proposals/{id}/approve",
    extensions(("x-permission" = json!("console.devices.manage"))),
    security(("session_cookie" = [])),
    params(
        ("id" = String, Path, description = "The proposal's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
        ("connection" = String, Query, description = "How the device is attached: `usb`, `network` \
                                                    or `serial` (ADR-0100). Required: discovery \
                                                    cannot find it out, and it decides whether a \
                                                    cash drawer may be opened"),
        ("station_id" = Option<String>, Query, description = "The kitchen station the device serves \
                                                            (a 26-character ULID). Absent, it is \
                                                            the counter's receipt printer"),
    ),
    responses(
        (status = 204, description = "No proposal with that id is pending any more. Idempotent: the \
                                      answer is the same whether one was, and only an approval that \
                                      acted is audited. The device reaches the store on the next \
                                      `POST /admin/devices/publish`"),
        (status = 400, description = "tenant_id, id or station_id is not a ULID, or connection is \
                                      absent or not one of the three", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The device store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "device onboarding",
)]
/// A super-admin approves a pending proposal.
pub(super) async fn approve_device<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<DeviceTenantQuery>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    resolve_device(&state, &headers, &id, &query, true).await
}

#[utoipa::path(
    post,
    path = "/admin/devices/proposals/{id}/reject",
    extensions(("x-permission" = json!("console.devices.manage"))),
    security(("session_cookie" = [])),
    params(
        ("id" = String, Path, description = "The proposal's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "No proposal with that id is pending any more. Idempotent: the \
                                      answer is the same whether one was, and only a rejection that \
                                      acted is audited"),
        (status = 400, description = "tenant_id or id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The device store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "device onboarding",
)]
/// A super-admin rejects a pending proposal.
pub(super) async fn reject_device<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<DeviceTenantQuery>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    resolve_device(&state, &headers, &id, &query, false).await
}

/// Resolves a proposal to approved or rejected (super-admin only). `204` whether or not a pending row
/// was found — resolving is idempotent, tenant-scoped, and telling the caller which case it was is a
/// needless enumeration signal.
async fn resolve_device<D, A, K, C>(
    state: &DeviceState<D, A, K, C>,
    headers: &HeaderMap,
    id: &str,
    query: &DeviceTenantQuery,
    approved: bool,
) -> Response
where
    D: DeviceProposalStore,
    A: AdminStore,
    C: ClockSource,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, id) = match parse_ulid_fields([("tenant_id", &query.tenant_id), ("id", id)]) {
        Ok([tenant_id, id]) => (TenantId::new(tenant_id), DeviceProposalId::new(id)),
        Err(refusal) => return refusal,
    };
    // Approval carries the two facts discovery cannot find (ADR-0100). A rejection carries neither:
    // they describe a device the store will address, and a rejected one never will.
    let (connection, station) = if approved {
        let Some(raw) = query.connection.as_deref() else {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "connection is required to approve a device: usb, network, or serial",
                &[("connection", "REQUIRED")],
            );
        };
        let Some(connection) = DeviceConnection::from_short_name(raw) else {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "connection must be one of usb, network, serial",
                &[("connection", "INVALID_ENUM_VALUE")],
            );
        };
        let station = match query.station_id.as_deref() {
            None => None,
            Some(raw) => match parse_ulid_fields([("station_id", raw)]) {
                Ok([station]) => Some(StationId::new(station)),
                Err(refusal) => return refusal,
            },
        };
        (Some(connection), station)
    } else {
        (None, None)
    };
    match state
        .devices
        .resolve(tenant_id, id, approved, connection, station)
        .await
    {
        Ok(found) => {
            // Only a resolve that actually acted on a pending proposal is worth an entry; resolving
            // an already-resolved id is an idempotent `204` and records nothing. `resolved_by` is the
            // acting admin, snapshotted onto the entry by `audit_action`.
            if found {
                audit_action(
                    &state.audit,
                    &state.clock,
                    &context,
                    Some(tenant_id),
                    if approved {
                        "device_proposal.approve"
                    } else {
                        "device_proposal.reject"
                    },
                    "device_proposal",
                    &id.to_string(),
                    None,
                    None,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => device_error_response(&error),
    }
}
