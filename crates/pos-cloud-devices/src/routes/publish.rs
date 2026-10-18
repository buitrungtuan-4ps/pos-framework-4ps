// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/devices/publish`: a store's approved devices compiled into its `devices` node
//! (ADR-0100).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::devices::{DeviceConnection, PublishedDevice, PublishedDevices};
use pos_proto::ids::{DeviceId, StationId, StoreId, TenantId};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::devices::{
    DeviceKind, DeviceProposalStatus, DeviceProposalStore, DeviceProposalSummary,
};
use pos_cloud_config::publish::publish_config_nodes;

/// The device-publish sub-router's state: the approval queue to read, the config tree to write.
#[derive(Clone)]
struct DevicePublishState<D, Cfg, A, C> {
    devices: D,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin selects the (tenant, store) whose `devices` node to compile and publish.
#[derive(Debug, Clone, Deserialize)]
struct PublishDevicesRequest {
    /// The tenant that owns the store (a 26-character ULID).
    tenant_id: String,
    /// The store whose approved devices to publish (a 26-character ULID).
    store_id: String,
}

/// Builds the device-publish sub-router
/// ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md), C2 slice 2b).
///
/// Separate from [`device_router`](crate::routes::device_router) because it needs the config tree, which the propose/approve
/// surface does not — the same split `pos-cloud`'s `floor_publish_router` makes for the same reason.
pub fn device_publish_router<D, Cfg, A, C>(
    devices: D,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/devices/publish",
            post(admin_publish_devices::<D, Cfg, A, C>),
        )
        .with_state(DevicePublishState {
            devices,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Compiles a store's **approved** devices into its `devices` config node and versions it
/// ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)).
///
/// The same load→compile→write→version shape as the floor publish beside it, onto the `devices` key.
/// A device whose stored `kind` or `connection` this build does not know keeps its token through
/// `Open` rather than failing the publish — the store on the other end applies the same rule, and a
/// node that refused to compile over one unfamiliar device would take a shop's receipt printer with
/// it.
///
/// A proposal with **no** connection is skipped, not published as a guess. That state is only
/// reachable for a row approved before ADR-0100 (the route now requires it), and publishing it as
/// `network` would silently disable the cash drawer on a USB printer. Skipping is visible: the
/// response says how many were published and how many were held back.
async fn admin_publish_devices<D, Cfg, A, C>(
    State(state): State<DevicePublishState<D, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishDevicesRequest>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
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

    let approved = match state
        .devices
        .list(tenant_id, Some(store_id), DeviceProposalStatus::Approved)
        .await
    {
        Ok(approved) => approved,
        Err(error) => {
            tracing::warn!(%error, "could not read the approved devices to publish");
            return service_unavailable("devices");
        }
    };
    let considered = approved.len();
    let node = compile_devices(&approved);
    if let Some((printer, agent)) = unresolvable_agent(&node) {
        return api_error_with_details(
            ErrorStatus::Unprocessable,
            "a device names a print agent that is not a terminal in this store's approved devices:              create the terminal, or clear the agent, then publish again",
            &[
                ("agent_device_id", &agent.to_string()),
                ("device_id", &printer.to_string()),
            ],
        );
    }
    let published_count = node.devices().len();

    let Ok(devices_value) = serde_json::to_value(&node) else {
        tracing::error!("could not serialise a compiled device node");
        return service_unavailable("devices");
    };

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        vec![("devices".to_owned(), devices_value)],
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "devices.publish",
        "store",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({
            "config_version_id": id.to_string(),
            "device_count": published_count,
            "skipped_count": considered.saturating_sub(published_count),
        })),
    )
    .await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "config_version_id": id.to_string(),
            "device_count": published_count,
            "skipped_count": considered.saturating_sub(published_count),
        })),
    )
        .into_response()
}

/// Turns approved proposals into the published node.
///
/// The proposal's ULID becomes the device's — the approval is what turns a proposal into a device,
/// and reusing the identifier means the audit trail on the proposal and the device the store
/// addresses are the same thing, rather than two ids an operator has to correlate by hand.
///
/// A row with no connection is dropped rather than guessed at; see [`admin_publish_devices`].
/// A `terminal` is the one exception, and the exception does not widen the rule: that rule exists to
/// stop a USB printer's cash drawer being silently published as a network device's, and a terminal
/// has neither a drawer nor an address to guess. Nothing dials it — the agent connects outbound to
/// the edge ([ADR-0112](../../../docs/adr/0112-print-agents.md)) — so `DEVICE_CONNECTION_UNSPECIFIED`
/// is not a guess here, it is the accurate answer.
pub(super) fn compile_devices(approved: &[DeviceProposalSummary]) -> PublishedDevices {
    PublishedDevices::new(with_listed_receipt_printers(
        approved
            .iter()
            .filter_map(|row| {
                let device_id = row.id.parse::<Ulid>().ok().map(DeviceId::new)?;
                let connection = match row.connection.as_deref() {
                    Some(connection) => Open::parse(&device_connection_token(connection)),
                    None if row.kind == DeviceKind::Terminal.as_wire() => {
                        Open::from_known(DeviceConnection::Unspecified)
                    }
                    None => return None,
                };
                let station_id = match row.station_id.as_deref() {
                    None => None,
                    // A station id that will not parse drops the *station*, not the device: the
                    // printer still exists and still prints, it simply falls back to serving no
                    // station until the approval is corrected.
                    Some(raw) => raw.parse::<Ulid>().ok().map(StationId::new),
                };
                let agent_device_id = match row.agent_device_id.as_deref() {
                    None => None,
                    // Same rule as the station, and the same safe direction: an agent id that will
                    // not parse drops the *agent*, so the edge opens the address itself — which is
                    // what it did before this field existed. In-store that is unchanged behaviour;
                    // on a hosted edge it is a named `PRINTER_UNAVAILABLE` rather than silence.
                    Some(raw) => raw.parse::<Ulid>().ok().map(DeviceId::new),
                };
                Some(PublishedDevice {
                    device_id,
                    kind: Open::parse(&device_kind_token(&row.kind)),
                    connection,
                    // Verbatim, including a terminal's — `create_terminal` is the only writer of a
                    // terminal row and writes an empty one, and there is no route that can set an
                    // address on it. Blanking it here would make this function invent a value
                    // instead of translating one.
                    address: row.address.clone(),
                    name: DisplayName::new(row.name.as_str()),
                    station_id,
                    agent_device_id,
                    // Verbatim. The edge decides whether a marked printer may open its drawer (USB
                    // only, its own transport only), so a mark on a network printer is published
                    // and inert rather than silently dropped here.
                    drawer_attached: row.drawer_attached,
                    // Verbatim, and absent where nobody has said: the edge reads absent as the 80 mm
                    // printer with a cutter every store assumed before the console could say
                    // (ADR-0160), so a store nobody has set publishes the node it did before.
                    paper_width: row
                        .paper_width
                        .as_deref()
                        .map_or_else(Open::default, Open::parse),
                    cuts_paper: row.cuts_paper,
                    // Verbatim, as the paper is, and absent where nobody has said: the edge reads
                    // absent as the store's receipt printer and languages (ADR-0160 decision 4). A
                    // printer id that will not parse drops the printer, as an agent's does, and one
                    // that names no printer of this node is dropped below.
                    receipt_printer_id: row
                        .receipt_printer_id
                        .as_deref()
                        .and_then(|raw| raw.parse::<Ulid>().ok())
                        .map(DeviceId::new),
                    receipt_language: row
                        .receipt_language
                        .as_deref()
                        .map_or_else(Open::default, Open::parse),
                    receipt_second_language: row
                        .receipt_second_language
                        .as_deref()
                        .map_or_else(Open::default, Open::parse),
                    // Verbatim, as the receipts are, and absent where nobody has said: the edge
                    // reads absent as the store's float (ADR-0167 decision 4), so a store that
                    // sets none publishes the node it did before.
                    opening_float_minor: row.opening_float_minor,
                })
            })
            .collect(),
    ))
}

/// `devices` with each till's `receipt_printer_id` that names no printer of `devices` serving no
/// station left out (ADR-0160 decision 4).
///
/// The rule for a till whose receipt printer goes: the till is published without the field, and
/// prints at the store's receipt printer, rather than the publish being refused, because the store
/// must keep printing. A printer goes when it is no longer approved, which archiving one would be,
/// or when its row cannot be published, as one approved with no connection cannot. The till's
/// languages stay, and the console still shows the printer it names, so an operator can choose
/// another.
fn with_listed_receipt_printers(mut devices: Vec<PublishedDevice>) -> Vec<PublishedDevice> {
    let receipt_printers: BTreeSet<DeviceId> = devices
        .iter()
        .filter(|device| {
            device.kind.known() == pos_proto::devices::DeviceKind::Printer
                && device.station_id.is_none()
        })
        .map(|device| device.device_id)
        .collect();
    for device in &mut devices {
        if let Some(printer) = device
            .receipt_printer_id
            .filter(|printer| !receipt_printers.contains(printer))
        {
            tracing::warn!(
                till = %device.device_id,
                %printer,
                "a till names a receipt printer the store's devices do not list; it is published \
                 printing at the store's receipt printer"
            );
            device.receipt_printer_id = None;
        }
    }
    devices
}

/// The first device in `node` that names an agent the node cannot resolve to a `TERMINAL`.
///
/// The console's half of ADR-0112's two gates. It is checked here, over the compiled node, rather
/// than at the moment an operator picks an agent, because *resolvable* is a property of the set
/// being published and not of either row alone: the terminal may be approved after the pick, or
/// archived before the publish, and only the compile sees both at once.
///
/// The two identity spaces this record spans make the precision matter. A paired device — a phone a
/// waiter signed in on — is a pairing, not an approved device, so it has no entry here and no
/// printer can name it. Refusing the *publish* is what makes that true at the store: an
/// unresolvable reference never leaves the cloud, so the edge never has to decide what to do with
/// one.
fn unresolvable_agent(node: &PublishedDevices) -> Option<(DeviceId, DeviceId)> {
    let terminals: BTreeSet<DeviceId> = node
        .devices()
        .iter()
        .filter(|device| device.kind.known() == pos_proto::devices::DeviceKind::Terminal)
        .map(|device| device.device_id)
        .collect();
    node.devices().iter().find_map(|device| {
        let agent = device.agent_device_id?;
        (!terminals.contains(&agent)).then_some((device.device_id, agent))
    })
}

/// The node's prefixed token for a stored short kind name (`printer` → `DEVICE_KIND_PRINTER`).
///
/// A name this build does not know still produces a token, which `Open` then retains — the
/// forward-compatibility the node promises has to start here, at the one place the two spellings
/// meet, or an unfamiliar device is lost before it reaches the store.
fn device_kind_token(short: &str) -> String {
    format!("DEVICE_KIND_{}", short.to_ascii_uppercase())
}

/// As [`device_kind_token`], for a connection (`usb` → `DEVICE_CONNECTION_USB`).
fn device_connection_token(short: &str) -> String {
    format!("DEVICE_CONNECTION_{}", short.to_ascii_uppercase())
}
