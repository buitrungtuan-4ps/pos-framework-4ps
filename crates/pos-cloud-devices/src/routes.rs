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
use crate::devices::{
    DeviceProposalId, DeviceProposalStatus, DeviceProposalStore, DeviceWriteOutcome,
};
use pos_cloud_api::conditional::version_mismatch;
use pos_cloud_api::refusal::not_found;

mod activation;
mod claims;
mod printers;
mod proposals;
mod publish;
mod revoke;
mod terminals;

pub use activation::activation_router;
pub use claims::{CLAIM_PATH, claim_router};
pub use publish::device_publish_router;
pub use revoke::device_revoke_router;

// The handlers the console's OpenAPI document names.
pub use activation::{__path_admin_issue_activation_code, __path_admin_revoke_activation_codes};
pub use claims::__path_admin_bind_claim;
pub use printers::{
    __path_admin_set_printer_paper, __path_set_drawer_attached, __path_set_print_agent,
};
pub use proposals::{__path_approve_device, __path_list_pending_devices, __path_reject_device};
pub use publish::__path_admin_publish_devices;
// And the ones the device document names (ADR-0177).
pub use activation::__path_exchange_activation_code;
pub use claims::{__path_collect_claim, __path_open_claim};
pub use proposals::{__path_list_store_devices, __path_propose_device};
pub use revoke::__path_admin_revoke_device;
pub use terminals::{
    __path_admin_set_terminal_float, __path_admin_set_terminal_receipt, __path_create_terminal,
};

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

/// Builds the device-onboarding sub-router, stated independently of `pos-cloud`'s `CloudApp`
/// ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
///
/// A store proposes a discovered printer/KDS and reads back its approved devices on the store-facing
/// `/sync` surface (API key, `manage_devices` scope); a super-admin lists the pending queue and
/// approves or rejects on `/admin` (session guard). It needs the proposal store plus the existing
/// admin/api-key/clock collaborators, so — like `pos-cloud`'s `reconcile_router` — it carries its own state and is
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
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct ProposeDeviceRequest {
    /// `printer` or `kds`.
    kind: String,
    /// A human-readable name for the device.
    name: String,
    /// The device's network address, as discovered (e.g. `192.168.1.50:9100`).
    address: String,
}

/// The id and status of a freshly-proposed device (always `pending`).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct ProposeDeviceResponse {
    /// The proposal's id (a ULID).
    id: String,
    /// The status — `pending` until an operator resolves it.
    status: DeviceProposalStatus,
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

#[cfg(test)]
mod devices_compile_tests {
    //! What a till's receipts compile to ([`super::compile_devices`], ADR-0160 decision 4), and the
    //! rule for a till whose receipt printer leaves the store's approved devices: it is published
    //! without the field, and prints at the store's receipt printer. And what a till's float compiles
    //! to (ADR-0167 decision 4): the float where one is set, and not a byte where none is.
    use super::publish::compile_devices;
    use crate::devices::DeviceProposalSummary;
    use pos_proto::devices::PublishedDevice;
    use pos_proto::ids::DeviceId;
    use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};
    use pos_proto::ulid::Ulid;

    /// An approved row of the one store, as the device store lists it.
    fn row(
        id: u128,
        kind: &str,
        connection: Option<&str>,
        station: Option<u128>,
    ) -> DeviceProposalSummary {
        DeviceProposalSummary {
            id: Ulid::from_u128(id).to_string(),
            store_id: Ulid::from_u128(0x5).to_string(),
            kind: kind.to_owned(),
            name: format!("Device {id}"),
            address: String::new(),
            connection: connection.map(str::to_owned),
            station_id: station.map(|station| Ulid::from_u128(station).to_string()),
            agent_device_id: None,
            drawer_attached: false,
            paper_width: None,
            cuts_paper: None,
            receipt_printer_id: None,
            receipt_language: None,
            receipt_second_language: None,
            opening_float_minor: None,
            status: "approved".to_owned(),
            version: "1".to_owned(),
        }
    }

    /// The bar till, naming `printer` and printing in English then Vietnamese.
    fn bar_till(printer: u128) -> DeviceProposalSummary {
        DeviceProposalSummary {
            receipt_printer_id: Some(Ulid::from_u128(printer).to_string()),
            receipt_language: Some("RECEIPT_LANGUAGE_EN".to_owned()),
            receipt_second_language: Some("RECEIPT_SECOND_LANGUAGE_VI".to_owned()),
            ..row(7, "terminal", None, None)
        }
    }

    /// The compiled entry for `id`.
    fn compiled(rows: &[DeviceProposalSummary], id: u128) -> PublishedDevice {
        compile_devices(rows)
            .devices()
            .iter()
            .find(|device| device.device_id == DeviceId::new(Ulid::from_u128(id)))
            .cloned()
            .expect("the device is published")
    }

    #[test]
    fn a_till_carries_its_receipt_printer_and_languages_and_one_nobody_set_carries_none() {
        let rows = [
            row(1, "printer", Some("network"), None),
            row(2, "printer", Some("network"), None),
            bar_till(2),
            row(8, "terminal", None, None),
        ];
        let bar = compiled(&rows, 7);
        assert_eq!(
            bar.receipt_printer_id,
            Some(DeviceId::new(Ulid::from_u128(2)))
        );
        assert_eq!(bar.receipt_language(), Some(ReceiptLanguage::English));
        assert_eq!(
            bar.receipt_second_language(),
            Some(ReceiptSecondLanguage::Vietnamese)
        );
        let door = serde_json::to_value(compiled(&rows, 8)).expect("json");
        for field in [
            "receipt_printer_id",
            "receipt_language",
            "receipt_second_language",
        ] {
            assert!(door.get(field).is_none(), "{field} is not written: {door}");
        }
    }

    #[test]
    fn a_till_whose_receipt_printer_goes_is_published_without_it_and_keeps_its_languages() {
        // Archived: the printer is no longer among the approved rows. A station's printer, and a
        // printer approved before a connection was recorded, which the publish holds back, are not
        // a counter's receipt printer either. Each till prints at the store's instead.
        for rows in [
            vec![row(1, "printer", Some("network"), None), bar_till(2)],
            vec![row(2, "printer", Some("network"), Some(0x5A)), bar_till(2)],
            vec![row(2, "printer", None, None), bar_till(2)],
            vec![row(2, "kds", Some("network"), None), bar_till(2)],
        ] {
            let bar = compiled(&rows, 7);
            assert_eq!(bar.receipt_printer_id, None, "{rows:?}");
            assert_eq!(bar.receipt_language(), Some(ReceiptLanguage::English));
            assert_eq!(
                bar.receipt_second_language(),
                Some(ReceiptSecondLanguage::Vietnamese)
            );
        }
    }

    #[test]
    fn a_till_carries_its_own_float_and_one_nobody_set_carries_no_key() {
        // `0` is a float, a drawer started empty, and not the store's.
        let float = |id, minor| DeviceProposalSummary {
            opening_float_minor: Some(minor),
            ..row(id, "terminal", None, None)
        };
        let rows = [
            float(7, 250_000),
            float(8, 0),
            row(9, "terminal", None, None),
        ];
        assert_eq!(compiled(&rows, 7).opening_float_minor(), Some(250_000));
        assert_eq!(compiled(&rows, 8).opening_float_minor(), Some(0));
        let door = serde_json::to_value(compiled(&rows, 9)).expect("json");
        assert!(door.get("opening_float_minor").is_none(), "{door}");
    }

    #[test]
    fn a_node_whose_tills_set_no_float_is_the_bytes_it_was_before_the_column() {
        // Migration 0085 leaves every row's float null, so a store nobody has set a float for
        // publishes the devices node it published before, byte for byte.
        let rows = [
            row(1, "printer", Some("network"), None),
            row(7, "terminal", None, None),
        ];
        let json = serde_json::to_string(&compile_devices(&rows)).expect("json");
        assert_eq!(
            json,
            concat!(
                r#"{"devices":[{"device_id":"00000000000000000000000001","#,
                r#""kind":"DEVICE_KIND_PRINTER","connection":"DEVICE_CONNECTION_NETWORK","#,
                r#""address":"","name":"Device 1"},"#,
                r#"{"device_id":"00000000000000000000000007","kind":"DEVICE_KIND_TERMINAL","#,
                r#""connection":"DEVICE_CONNECTION_UNSPECIFIED","address":"","name":"Device 7"}]}"#,
            )
        );
    }
}

#[cfg(test)]
mod revoked_devices_tests {
    //! The deny-list a revocation writes ([`super::append_revoked_device`]): the list the store runs,
    //! read from the tree's layers composed, with the new id appended.
    use super::revoke::append_revoked_device;
    use crate::config_tree::ConfigTreeState;
    use pos_core::device_revocation::MAX_REVOKED_DEVICES;
    use pos_proto::ulid::Ulid;
    use serde_json::{Value, json};

    /// A tree whose Tenant layer holds `tenant` and whose Store layer holds `store`.
    fn tree(tenant: Value, store: Value) -> ConfigTreeState {
        ConfigTreeState {
            layers: [tenant, json!({}), store, json!({})],
            history: Vec::new(),
            k: 8,
        }
    }

    fn id(n: u128) -> String {
        Ulid::from_u128(n).to_string()
    }

    fn deny_list(ids: &[String]) -> Value {
        json!({ "revoked_devices": { "device_ids": ids } })
    }

    /// The ids the append writes onto the Store layer.
    fn written(before: &ConfigTreeState, listed: &str) -> Vec<String> {
        let (key, node) = append_revoked_device(Some(before), listed)
            .expect("appended")
            .into_iter()
            .next()
            .expect("one node");
        assert_eq!(key, "revoked_devices");
        serde_json::from_value(node["device_ids"].clone()).expect("a list of ids")
    }

    #[test]
    fn a_restored_list_is_kept_and_the_new_id_appended_to_it() {
        // A rollback leaves the restored list on the Tenant layer and the Store layer empty.
        let restored = tree(deny_list(&[id(1), id(2)]), json!({}));
        assert_eq!(written(&restored, &id(3)), vec![id(1), id(2), id(3)]);
        assert_eq!(
            written(&restored, &id(1)),
            vec![id(1), id(2)],
            "an id already listed adds nothing"
        );
    }

    #[test]
    fn the_list_appended_to_is_the_one_the_store_runs() {
        // A list on the Store layer replaces one below it when the layers merge, so it is the list
        // the store runs, and the one a revocation adds to.
        let both = tree(deny_list(&[id(1)]), deny_list(&[id(2)]));
        assert_eq!(written(&both, &id(3)), vec![id(2), id(3)]);
    }

    #[test]
    fn the_cap_counts_the_list_the_store_runs() {
        let full: Vec<String> = (1_u128..).take(MAX_REVOKED_DEVICES).map(id).collect();
        let restored = tree(deny_list(&full), json!({}));
        let refused =
            append_revoked_device(Some(&restored), &id(0xFFFF)).expect_err("past the cap");
        assert_eq!(refused.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            written(&restored, &id(1)).len(),
            MAX_REVOKED_DEVICES,
            "an id already listed re-publishes the full list"
        );
    }
}
