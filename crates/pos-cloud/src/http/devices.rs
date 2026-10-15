// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Devices: a store's onboarding of the devices it finds (`/sync/.../devices`,
//! `/admin/devices/proposals`), the terminals and printer settings an operator writes, the device
//! publish, revocation, activation codes and claims.
//!
//! The state, the request bodies the store sends, the router and the write and error helpers are here.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pos_cloud_api::console::audit_action;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;
use printers::{admin_set_printer_paper, set_drawer_attached, set_print_agent};
use proposals::{
    approve_device, list_pending_devices, list_store_devices, propose_device, reject_device,
};
use serde::Deserialize;
use terminals::{admin_set_terminal_float, admin_set_terminal_receipt, create_terminal};

use crate::audit::AuditRecorder;
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::apikey::ApiKeyStore;
use crate::devices::{DeviceProposalId, DeviceProposalStore, DeviceWriteOutcome};
use crate::http::{not_found, version_mismatch};

mod activation;
mod admitted;
mod claims;
mod printers;
mod proposals;
// `pub(super)`: the unit tests in `http.rs` reach the device compile and the revocation deny-list.
pub(super) mod publish;
pub(super) mod revoke;
mod terminals;

pub use activation::activation_router;
// The admitted-device roster the main router mounts.
pub(crate) use admitted::__path_admin_admitted_devices;
pub(super) use admitted::admin_admitted_devices;
pub use claims::claim_router;
pub use publish::device_publish_router;
pub use revoke::device_revoke_router;

// The handlers the console's OpenAPI document names.
pub(crate) use printers::__path_admin_set_printer_paper;
pub(crate) use revoke::__path_admin_revoke_device;
pub(crate) use terminals::{__path_admin_set_terminal_float, __path_admin_set_terminal_receipt};

// --- Device onboarding (`/sync/.../devices` + `/admin/devices/proposals`) ------------------------

/// The collaborators the device-onboarding routes need, stated independently of [`CloudApp`]: the
/// proposal store, plus the admin, API-key, and clock stores the two auth paths use.
#[derive(Clone)]
struct DeviceState<D, A, K, C> {
    devices: D,
    admin: A,
    keys: K,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the device-onboarding sub-router, stated independently of [`CloudApp`](crate::http::CloudApp)
/// ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
///
/// A store proposes a discovered printer/KDS and reads back its approved devices on the store-facing
/// `/sync` surface (API key, `manage_devices` scope); a super-admin lists the pending queue and
/// approves or rejects on `/admin` (session guard). It needs the proposal store plus the existing
/// admin/api-key/clock collaborators, so — like [`reconcile_router`](crate::http::reconcile_router) — it carries its own state and is
/// merged into the main router rather than adding an eighth `CloudApp` generic.
pub fn device_router<D, A, K, C>(
    devices: D,
    admin: A,
    keys: K,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/devices",
            post(propose_device::<D, A, K, C>).get(list_store_devices::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals",
            get(list_pending_devices::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/approve",
            post(approve_device::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/reject",
            post(reject_device::<D, A, K, C>),
        )
        .route(
            "/admin/devices/terminals",
            post(create_terminal::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/agent",
            post(set_print_agent::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/drawer",
            post(set_drawer_attached::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/paper",
            post(admin_set_printer_paper::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/receipt",
            post(admin_set_terminal_receipt::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/float",
            post(admin_set_terminal_float::<D, A, K, C>),
        )
        .with_state(DeviceState {
            devices,
            admin,
            keys,
            clock,
            audit,
        })
}

/// A store's proposal of a discovered device.
#[derive(Debug, Clone, Deserialize)]
struct ProposeDeviceRequest {
    /// `printer` or `kds`.
    kind: String,
    /// A human-readable name for the device.
    name: String,
    /// The device's network address, as discovered (e.g. `192.168.1.50:9100`).
    address: String,
}

/// The id and status of a freshly-proposed device (always `pending`).
#[derive(Debug, Clone, serde::Serialize)]
struct ProposeDeviceResponse {
    /// The proposal's id (a ULID).
    id: String,
    /// The status — `pending` until an operator resolves it.
    status: String,
}

/// The tenant a device listing or resolution is scoped to (the super-admin is global).
#[derive(Debug, Clone, Deserialize)]
struct DeviceTenantQuery {
    /// The tenant whose proposals to act on (a 26-character ULID).
    tenant_id: String,
    /// How an **approved** device is attached: `usb`, `network` or `serial`
    /// ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)). Required on approve,
    /// ignored on reject. Discovery cannot find this out, and it decides whether a cash drawer may
    /// be opened at all, so approval is where a human states it.
    #[serde(default)]
    connection: Option<String>,
    /// The kitchen station an **approved** device serves (a ULID). Absent means the counter's
    /// receipt printer, which serves the bill rather than a station. Ignored on reject.
    #[serde(default)]
    station_id: Option<String>,
    /// Narrow a **listing** to one store (a ULID). Absent lists the tenant's whole fleet, which is
    /// what the onboarding queue wants; the print-agent picker wants one store's devices, because an
    /// agent may only be a terminal standing in the same shop
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    #[serde(default)]
    store_id: Option<String>,
    /// Which **status** to list: `pending` (the default, and the onboarding queue's whole subject),
    /// `approved` or `rejected`.
    ///
    /// Defaulting to `pending` is what keeps this additive: the read shipped serving pending rows
    /// only, and every existing caller keeps getting exactly those. The picker needs `approved`,
    /// because `set_print_agent` acts on a printer that is already in service and names a terminal
    /// that is already created — and until this parameter existed there was no `/admin` read that
    /// could show either, so no console could drive that route at all.
    #[serde(default)]
    status: Option<String>,
}

/// The answer to a conditional write on an approved device: an agent pick, a drawer mark, a
/// printer's paper, a till's receipts or its float.
///
/// A write that changed the row is audited as `action`, with what the device now says as the
/// entry's `after`: each of them decides what a store does with real paper or real cash. A stale
/// version is a `412` and a device that is not there a `404`, as for every conditional write.
async fn device_write_response<D, A, K, C>(
    state: &DeviceState<D, A, K, C>,
    context: &AdminContext,
    (tenant_id, id): (TenantId, DeviceProposalId),
    action: &str,
    after: serde_json::Value,
    written: Result<DeviceWriteOutcome, crate::devices::DeviceProposalError>,
) -> Response
where
    C: ClockSource,
{
    match written {
        Ok(DeviceWriteOutcome::Updated) => {
            audit_action(
                &state.audit,
                &state.clock,
                context,
                Some(tenant_id),
                action,
                "device_proposal",
                &id.to_string(),
                None,
                Some(after),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(DeviceWriteOutcome::VersionMismatch) => version_mismatch(),
        Ok(DeviceWriteOutcome::NotFound) => not_found("device"),
        Err(error) => device_error_response(&error),
    }
}

/// Maps a device-proposal store failure to a retryable `503`, logging the detail rather than leaking it.
fn device_error_response(error: &crate::devices::DeviceProposalError) -> Response {
    tracing::error!(%error, "a device-proposal store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the device service is unavailable",
    )
}
