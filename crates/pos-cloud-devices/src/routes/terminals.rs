// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The terminals an operator names, and each till's receipt and float settings.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};
use pos_proto::{ErrorStatus, WireEnum};
use serde::Deserialize;

use super::{DeviceState, device_error_response, device_write_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::devices::{
    DeviceKind, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore, DeviceProposalSummary,
    PersistedDeviceProposal,
};
use pos_cloud_api::conditional::if_match;
use pos_cloud_api::refusal::{not_found, optional_choice};

/// An operator names a POS terminal that exists because somebody carried it into the shop.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateTerminalRequest {
    /// The tenant the store belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store the terminal stands in (a ULID).
    store_id: String,
    /// What to call it on screen when it is the machine that went quiet.
    name: String,
}

/// The id a freshly created terminal was given.
#[derive(Debug, Clone, serde::Serialize)]
struct CreateTerminalResponse {
    /// The terminal's device id (a ULID) — what a printer names to make it its agent.
    id: String,
    /// `approved`, always: a terminal is created resolved, because the console write *is* the
    /// decision.
    status: String,
}

#[utoipa::path(
    post,
    path = "/admin/devices/terminals",
    extensions(("x-permission" = json!("console.devices.manage"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id`, the `store_id` the terminal stands in, and its `name`, what an \
                       operator recognises it by. A terminal is never discovered, so there is no \
                       proposal to approve: creating it is the decision (ADR-0112). It has no \
                       address, because nothing dials a terminal",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, approved. The body carries the terminal's `id`, what a \
                                      printer names to make it its print agent, and `status`. \
                                      Audited"),
        (status = 400, description = "tenant_id or store_id is not a ULID, or name is blank", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The device store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "device onboarding",
)]
/// A super-admin creates an approved `terminal` device
/// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
///
/// Behind `ManageDevices` rather than `PublishConfig`, and audited, because this is the human gate:
/// nothing on a LAN announces itself as a till, so there is no proposal for an operator to approve
/// and the named admin creating the entry is the decision ADR-0041 protects. It carries no
/// `connection` and no address — the agent connects outbound to the edge, and nothing dials a
/// terminal.
pub(super) async fn create_terminal<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateTerminalRequest>,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required: an operator has to recognise this machine when it goes quiet",
            &[("name", "REQUIRED")],
        );
    }
    let Some(id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DeviceProposalId::new)
    else {
        tracing::error!("could not read OS entropy to mint a terminal's device id");
        return service_unavailable("device");
    };
    let terminal = PersistedDeviceProposal {
        id,
        tenant_id,
        store_id,
        kind: DeviceKind::Terminal,
        name: request.name,
        // Nothing dials a terminal: the agent opens a connection to the edge, never the reverse.
        address: String::new(),
    };
    match state.devices.create_terminal(&terminal).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "device_terminal.create",
                "device_proposal",
                &id.to_string(),
                None,
                Some(serde_json::json!({
                    "store_id": store_id.to_string(),
                    "kind": DeviceKind::Terminal.as_wire(),
                })),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(CreateTerminalResponse {
                    id: id.to_string(),
                    status: DeviceProposalStatus::Approved.as_wire().to_owned(),
                }),
            )
                .into_response()
        }
        Err(error) => device_error_response(&error),
    }
}

/// An operator says where an approved terminal's receipts go and the languages they print in.
///
/// The three fields are the till's whole receipt state, as a station's update is its whole state: an
/// absent or `null` field is the store's again.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SetReceiptRequest {
    /// The tenant the terminal belongs to (a 26-character ULID).
    tenant_id: String,
    /// The printer this till's receipts, receipt copies and pre-bills go to (a ULID), or `null` for
    /// the store's receipt printer.
    #[serde(default)]
    receipt_printer_id: Option<String>,
    /// The language they print in, as a `RECEIPT_LANGUAGE_…` token, or `null` for the store's.
    #[serde(default)]
    receipt_language: Option<String>,
    /// The second language they print in, as a `RECEIPT_SECOND_LANGUAGE_…` token, or `null` for the
    /// store's.
    #[serde(default)]
    receipt_second_language: Option<String>,
}

#[utoipa::path(
    post,
    path = "/admin/devices/proposals/{id}/receipt",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.devices.manage"))),
    params(
        ("id" = String, Path, description = "The terminal's 26-character ULID"),
        ("If-Match" = String, Header, description = "The version the terminal was read at \
                                                     (ADR-0094). Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`; `receipt_printer_id`, an approved printer of the terminal's \
                       store that serves no station; `receipt_language`, one of \
                       `RECEIPT_LANGUAGE_DISPLAY`, `RECEIPT_LANGUAGE_COUNTRY`, \
                       `RECEIPT_LANGUAGE_VI` and `RECEIPT_LANGUAGE_EN`; and \
                       `receipt_second_language`, one of `RECEIPT_SECOND_LANGUAGE_NONE`, \
                       `RECEIPT_SECOND_LANGUAGE_VI` and `RECEIPT_SECOND_LANGUAGE_EN`. Each is \
                       `null` or absent for the store's: its receipt printer, \
                       `printing.receipt_language` and `printing.receipt_second_language`. The \
                       three are the till's whole receipt state, so a field left out is the \
                       store's again",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Saved. It reaches the store on the next devices publish, \
                                      `POST /admin/devices/publish`; an edge older than 0.14.1 \
                                      ignores it. A device paired at the store prints by it once \
                                      it is bound to the terminal, `POST /api/print/agent`"),
        (status = 400, description = "The device is not a terminal (`id`, `WRONG_KIND`), \
                                      `receipt_printer_id` is not an approved device of its store \
                                      (`UNKNOWN_REFERENCE`) or not a printer serving no station \
                                      (`WRONG_KIND`), a language is not one of its choices, an id \
                                      is not a ULID, or If-Match is absent, malformed, or a \
                                      wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no approved device with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The terminal changed since you read it: re-read it and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The device store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "printers",
)]
/// A super-admin says which printer a till's receipts, receipt copies and pre-bills go to, and the
/// languages they print in
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 4).
///
/// The paper's guards, for the paper's reasons: `ManageDevices`, an `If-Match`, and an audit entry.
/// Unlike an agent pick, the printer is checked here, against the tenant's approved devices: it must
/// be a printer of the terminal's own store that serves no station, because a kitchen's printer is
/// for its tickets and another store's is in another building. What the check cannot see is a
/// printer that leaves the approved set later: the publish then leaves that till printing at the
/// store's receipt printer.
pub(super) async fn admin_set_terminal_receipt<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SetReceiptRequest>,
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
    let printer = match request.receipt_printer_id.as_deref() {
        None => None,
        Some(raw) => match parse_ulid_fields([("receipt_printer_id", raw)]) {
            Ok([printer]) => Some(DeviceProposalId::new(printer)),
            Err(refusal) => return refusal,
        },
    };
    let language = match optional_choice::<ReceiptLanguage>(
        "receipt_language",
        request.receipt_language.as_deref(),
    ) {
        Ok(language) => language,
        Err(refusal) => return refusal,
    };
    let second_language = match optional_choice::<ReceiptSecondLanguage>(
        "receipt_second_language",
        request.receipt_second_language.as_deref(),
    ) {
        Ok(language) => language,
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let approved = match state
        .devices
        .list(tenant_id, None, DeviceProposalStatus::Approved)
        .await
    {
        Ok(approved) => approved,
        Err(error) => return device_error_response(&error),
    };
    if let Some(refusal) = till_receipt_refusal(&approved, id, printer) {
        return refusal;
    }
    let written = state
        .devices
        .set_receipt(
            tenant_id,
            id,
            printer,
            language,
            second_language,
            expected.as_str(),
        )
        .await;
    device_write_response(
        &state,
        &context,
        (tenant_id, id),
        "device_proposal.set_receipt",
        serde_json::json!({
            "receipt_printer_id": printer.map(|printer| printer.to_string()),
            "receipt_language": language.map(ReceiptLanguage::as_wire),
            "receipt_second_language": second_language.map(ReceiptSecondLanguage::as_wire),
        }),
        written,
    )
    .await
}

/// Why `id` may not have its receipts printed at `printer`, among the tenant's `approved` devices, or
/// `None` when it may.
///
/// A `404` for a device the tenant has not approved, as a conditional write answers, and a `400`
/// naming the field otherwise: `id` when the device is not a terminal, because only a till prints a
/// guest's paper for itself, and `receipt_printer_id` when the printer is not an approved device of
/// the terminal's store, or is one that is not a printer serving no station.
fn till_receipt_refusal(
    approved: &[DeviceProposalSummary],
    id: DeviceProposalId,
    printer: Option<DeviceProposalId>,
) -> Option<Response> {
    let id = id.to_string();
    let Some(terminal) = approved.iter().find(|row| row.id == id) else {
        return Some(not_found("device"));
    };
    if terminal.kind != DeviceKind::Terminal.as_wire() {
        return Some(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "only a terminal has a receipt printer and receipt languages of its own",
            &[("id", "WRONG_KIND")],
        ));
    }
    let printer = printer?.to_string();
    match approved
        .iter()
        .find(|row| row.id == printer && row.store_id == terminal.store_id)
    {
        None => Some(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "receipt_printer_id must name an approved printer in the terminal's store",
            &[("receipt_printer_id", "UNKNOWN_REFERENCE")],
        )),
        Some(row) if row.kind != DeviceKind::Printer.as_wire() || row.station_id.is_some() => {
            Some(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "receipt_printer_id must name a printer that serves no station: a station's \
                 printer prints that kitchen's tickets",
                &[("receipt_printer_id", "WRONG_KIND")],
            ))
        }
        Some(_) => None,
    }
}

/// An operator says what an approved terminal's drawer opens with by default.
///
/// The float is the till's whole float state, as the receipt fields are its receipt state: an absent
/// or `null` float is the store's again.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SetFloatRequest {
    /// The tenant the terminal belongs to (a 26-character ULID).
    tenant_id: String,
    /// The float, a whole amount in the store currency's minor unit, or `null` for the store's
    /// `shift.opening_float_minor`.
    #[serde(default)]
    opening_float_minor: Option<i64>,
}

#[utoipa::path(
    post,
    path = "/admin/devices/proposals/{id}/float",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.devices.manage"))),
    params(
        ("id" = String, Path, description = "The terminal's 26-character ULID"),
        ("If-Match" = String, Header, description = "The version the terminal was read at \
                                                     (ADR-0094). Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`; and `opening_float_minor`, a whole amount in the store \
                       currency's minor unit from `0` to `1000000000`, the bounds of \
                       `shift.opening_float_minor`, or `null` or absent for that setting's float. \
                       It is the till's whole float state, so a float left out is the store's \
                       again",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Saved. It reaches the store on the next devices publish, \
                                      `POST /admin/devices/publish`, and prefills the float the \
                                      till's drawer is started on; an edge older than 0.14.1 \
                                      ignores it and prefills the store's"),
        (status = 400, description = "The device is not a terminal (`id`, `WRONG_KIND`), the \
                                      float is outside its bounds (`opening_float_minor`, \
                                      `OUT_OF_RANGE`), an id is not a ULID, or If-Match is \
                                      absent, malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no approved device with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The terminal changed since you read it: re-read it and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The device store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "printers",
)]
/// A super-admin says what a till's drawer opens with by default
/// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 4).
///
/// The receipts' guards, for the receipts' reasons: `ManageDevices`, an `If-Match`, and an audit
/// entry, since the float is the cash the Shift screen proposes a till's drawer start with. The
/// bounds are `shift.opening_float_minor`'s, which pos-proto holds, so a till is never given a
/// float the store could not be; the edge reads one outside them as the store's anyway.
pub(super) async fn admin_set_terminal_float<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SetFloatRequest>,
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
    let bounds = pos_proto::shift::OPENING_FLOAT_MINOR;
    if let Some(minor) = request.opening_float_minor
        && !bounds.contains(&minor)
    {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!(
                "opening_float_minor must be from {} to {}",
                bounds.start(),
                bounds.end()
            ),
            &[("opening_float_minor", "OUT_OF_RANGE")],
        );
    }
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let approved = match state
        .devices
        .list(tenant_id, None, DeviceProposalStatus::Approved)
        .await
    {
        Ok(approved) => approved,
        Err(error) => return device_error_response(&error),
    };
    let terminal = id.to_string();
    match approved.iter().find(|row| row.id == terminal) {
        None => return not_found("device"),
        Some(row) if row.kind != DeviceKind::Terminal.as_wire() => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "only a terminal has a drawer, and so a float, of its own",
                &[("id", "WRONG_KIND")],
            );
        }
        Some(_) => {}
    }
    let written = state
        .devices
        .set_float(
            tenant_id,
            id,
            request.opening_float_minor,
            expected.as_str(),
        )
        .await;
    device_write_response(
        &state,
        &context,
        (tenant_id, id),
        "device_proposal.set_float",
        serde_json::json!({ "opening_float_minor": request.opening_float_minor }),
        written,
    )
    .await
}
