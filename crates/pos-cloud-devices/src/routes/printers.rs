// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An approved printer's settings: the terminal that holds its transport, its cash drawer and its
//! paper.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::determinism::ClockSource;
use pos_proto::devices::PaperWidth;
use pos_proto::ids::TenantId;
use pos_proto::{ErrorStatus, WireEnum};
use serde::Deserialize;

use super::{DeviceState, device_write_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::devices::{DeviceProposalId, DeviceProposalStore};
use pos_cloud_api::conditional::if_match;
use pos_cloud_api::refusal::{accepted_tokens, enum_refusal};

/// An operator points an approved device at the terminal that holds its transport, or clears it.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SetPrintAgentRequest {
    /// The tenant the device belongs to (a 26-character ULID).
    tenant_id: String,
    /// The terminal whose agent will write this printer's bytes (a ULID), or `null` to go back to
    /// the edge opening the address itself.
    #[serde(default)]
    agent_device_id: Option<String>,
}

/// A super-admin picks (or clears) the print agent for an approved device
/// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
///
/// Behind `ManageDevices` with an `If-Match`
/// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)) and an audit entry: two
/// managers picking different agents for one printer is the ordinary race, and last-write-wins would
/// leave the loser believing a decision that is not in the database.
///
/// It deliberately does **not** check that the id names a terminal. Resolvability is a property of
/// the set being published — the terminal may be created after this pick — so the check lives in
/// [`admin_publish_devices`], which sees the whole node at once and is the last gate before a store
/// hears about any of it.
pub(super) async fn set_print_agent<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SetPrintAgentRequest>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, id) = match parse_ulid_fields([("tenant_id", &request.tenant_id), ("id", &id)])
    {
        Ok([tenant_id, id]) => (TenantId::new(tenant_id), DeviceProposalId::new(id)),
        Err(refusal) => return refusal,
    };
    let agent = match request.agent_device_id.as_deref() {
        None => None,
        Some(raw) => match parse_ulid_fields([("agent_device_id", raw)]) {
            Ok([agent]) => Some(DeviceProposalId::new(agent)),
            Err(refusal) => return refusal,
        },
    };
    if agent == Some(id) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a device cannot be its own print agent",
            &[("agent_device_id", "SELF_REFERENCE")],
        );
    }
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let written = state
        .devices
        .set_agent(tenant_id, id, agent, expected.as_str())
        .await;
    device_write_response(
        &state,
        &context,
        (tenant_id, id),
        "device_proposal.set_agent",
        serde_json::json!({ "agent_device_id": agent.map(|agent| agent.to_string()) }),
        written,
    )
    .await
}

/// An operator marks an approved printer as having a cash drawer wired to it, or clears the mark.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SetDrawerRequest {
    /// The tenant the device belongs to (a 26-character ULID).
    tenant_id: String,
    /// Whether a cash drawer is on this printer's kick port.
    drawer_attached: bool,
}

/// A super-admin ticks (or clears) **Cash drawer attached** on an approved printer
/// ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)
/// decision 4).
///
/// The agent pick's guards, for the agent pick's reasons: `ManageDevices`, an `If-Match`, and an
/// audit entry. The mark decides whether a drawer full of cash can be sprung from the till, so who
/// set it, and when, is worth a row. It reaches a store on the next devices publish, as every other
/// fact about a device does.
pub(super) async fn set_drawer_attached<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SetDrawerRequest>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, id) = match parse_ulid_fields([("tenant_id", &request.tenant_id), ("id", &id)])
    {
        Ok([tenant_id, id]) => (TenantId::new(tenant_id), DeviceProposalId::new(id)),
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let written = state
        .devices
        .set_drawer(tenant_id, id, request.drawer_attached, expected.as_str())
        .await;
    device_write_response(
        &state,
        &context,
        (tenant_id, id),
        "device_proposal.set_drawer",
        serde_json::json!({ "drawer_attached": request.drawer_attached }),
        written,
    )
    .await
}

/// An operator says what paper an approved printer takes and whether it cuts it.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SetPaperRequest {
    /// The tenant the device belongs to (a 26-character ULID).
    tenant_id: String,
    /// The paper the printer takes, as its `PAPER_WIDTH_…` token.
    paper_width: String,
    /// Whether the printer cuts its paper: `false` for one with no cutter.
    cuts_paper: bool,
}

#[utoipa::path(
    post,
    path = "/admin/devices/proposals/{id}/paper",
    params(
        ("id" = String, Path, description = "The approved printer's 26-character ULID"),
        ("If-Match" = String, Header, description = "The version the printer was read at \
                                                     (ADR-0094). Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`; `paper_width`, one of `PAPER_WIDTH_MILLIMETRES_80` (42 \
                       characters a line and a 576-dot raster), \
                       `PAPER_WIDTH_MILLIMETRES_80_COLUMNS_48` (48 characters, 576 dots) and \
                       `PAPER_WIDTH_MILLIMETRES_58` (32 characters, 384 dots); and `cuts_paper`, \
                       `false` for a printer with no cutter. A printer nobody has set prints as \
                       80 mm paper with a cutter",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Saved. It reaches the store on the next devices publish, \
                                      `POST /admin/devices/publish`"),
        (status = 400, description = "`paper_width` is not one of the three, an id is not a \
                                      ULID, or If-Match is absent, malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no approved device with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The device changed since you read it: re-read it and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The device store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "printers",
)]
/// A super-admin says what paper an approved printer takes and whether it cuts it
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2).
///
/// The drawer mark's guards, for the drawer mark's reasons: `ManageDevices`, an `If-Match`, and an
/// audit entry. The width decides how a receipt is laid out and how wide a raster is drawn, and the
/// cutter whether a printer is sent a cut, so who set them is worth a row. They reach a store on the
/// next devices publish, as every other fact about a device does. `PAPER_WIDTH_UNSPECIFIED` is
/// refused with every token this release does not know: it is what an older reader takes a newer
/// paper to be, never a choice.
pub(super) async fn admin_set_printer_paper<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SetPaperRequest>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, id) = match parse_ulid_fields([("tenant_id", &request.tenant_id), ("id", &id)])
    {
        Ok([tenant_id, id]) => (TenantId::new(tenant_id), DeviceProposalId::new(id)),
        Err(refusal) => return refusal,
    };
    let paper_width = match PaperWidth::from_wire(&request.paper_width) {
        Some(known) if known != PaperWidth::Unspecified => known,
        _ => return enum_refusal("paper_width", accepted_tokens::<PaperWidth>()),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let written = state
        .devices
        .set_paper(
            tenant_id,
            id,
            paper_width,
            request.cuts_paper,
            expected.as_str(),
        )
        .await;
    device_write_response(
        &state,
        &context,
        (tenant_id, id),
        "device_proposal.set_paper",
        serde_json::json!({
            "paper_width": paper_width.as_wire(),
            "cuts_paper": request.cuts_paper,
        }),
        written,
    )
    .await
}
