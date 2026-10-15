// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Settling and retiring a store handover.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_core::lease::LeaseGeneration;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::{OtaConfigState, lease_store_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::not_found;
use crate::lease::{LeaseStore, RetireOutcome, SettleOutcome};
use crate::ota::ArtifactStore;

/// A `POST /admin/config/lease/settle` body: the store, and the generation the caller is attesting
/// has nothing left on it.
///
/// `superseded_generation` is required and is the whole precondition. It is not "which handover do
/// you mean" — one store has at most one in flight — it is the caller stating *which machine* they
/// checked. A settle that took no number would let a stale console close a handover the operator
/// never looked at.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SettleHandoverRequest {
    tenant_id: String,
    store_id: String,
    superseded_generation: u64,
}

/// A `POST /admin/config/lease/retire` body: which store's settled handover is being retired.
///
/// No generation, and that asymmetry with settle is deliberate. Settle is an attestation about one
/// named machine's disk; retire is a decision about the handover the row currently describes, and
/// the row's own preconditions — settled, and not already retired — are what make it unambiguous.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct RetireHandoverRequest {
    tenant_id: String,
    store_id: String,
}

/// `POST /admin/config/lease/settle` — a person attests that a superseded machine holds no events,
/// closing a handover that will never close itself. Behind [`ConsolePermission::ManageStores`].
///
/// The automatic path is a heartbeat from the outgoing machine reporting that generation with an
/// empty outbox ([ADR-0110](../../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)). This
/// route is for the machine that will never send one: a box already powered off, or one whose disk
/// an operator has read directly. It is an attestation, so it is audited with the attester's name.
///
/// Publishes nothing. A settled handover changes what the *console* knows, not what any till is
/// told — the lease generation the store runs on is untouched.
pub(super) async fn admin_settle_handover<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<SettleHandoverRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
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
    let superseded = LeaseGeneration::new(request.superseded_generation);
    match state
        .config_trees
        .settle_handover(tenant_id, store_id, superseded)
        .await
    {
        Ok(SettleOutcome::Settled) => {}
        Ok(SettleOutcome::NotSuperseded { current }) => {
            return api_error_with_details(
                ErrorStatus::Unprocessable,
                "this store's handover is not on the generation you named; re-read and check which \
                 machine you are attesting to",
                &[(
                    "superseded_generation",
                    &current.map_or_else(
                        || "NO_HANDOVER_IN_FLIGHT".to_owned(),
                        |generation| generation.value().to_string(),
                    ),
                )],
            );
        }
        Ok(SettleOutcome::NoLease) => {
            return not_found("store");
        }
        Err(error) => return lease_store_error_response(&error),
    }
    // After the write, and best-effort: a settle that succeeded must not be reported as a failure
    // because its trail entry could not be written (ADR-0069). `before` is the handover that was in
    // flight; `after` is `null`, because settling is the removal of it.
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.lease.settle",
        "store",
        &request.store_id,
        Some(serde_json::json!({ "superseded_generation": request.superseded_generation })),
        None,
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /admin/config/lease/retire` — a person decides a settled handover's outgoing machine, its
/// database and its hosting are no longer needed. Behind [`ConsolePermission::ManageStores`].
///
/// Nothing transitions into this on its own and nothing ever will: it is a judgement about money and
/// risk that no fact in this system implies
/// ([ADR-0110](../../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)). It refuses while a
/// handover is in flight, because a machine that may hold the only copy of a night's sales is not
/// one anybody can decide is unnecessary — settle it first, by draining it or by attesting it empty.
///
/// Publishes nothing, for the same reason settle does not.
pub(super) async fn admin_retire_handover<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<RetireHandoverRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
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
    let at = state.clock.now();
    match state
        .config_trees
        .retire(tenant_id, store_id, at, &context.admin.id)
        .await
    {
        Ok(RetireOutcome::Retired) => {}
        Ok(RetireOutcome::Undrained { superseded }) => {
            return api_error_with_details(
                ErrorStatus::Unprocessable,
                "this store's handover is still in flight, so its previous machine may still hold \
                 events; settle it before deciding it is no longer needed",
                &[("superseded_generation", &superseded.value().to_string())],
            );
        }
        // The decider's id, never their email: the trail carries the address, and a refusal body is
        // read by whoever asked, who may not be the person entitled to see it.
        Ok(RetireOutcome::AlreadyRetired { at, by }) => {
            return api_error_with_details(
                ErrorStatus::Unprocessable,
                "this store's handover has already been retired; the trail records who decided and \
                 when",
                &[
                    ("retired_at", &at.as_milliseconds_since_epoch().to_string()),
                    ("retired_by", &by),
                ],
            );
        }
        // Not a race and not a missing store: the store exists, holds its first-ever lease, and has
        // never had a second machine. `422` rather than `404` says exactly that — the resource is
        // there, the act does not apply to it.
        Ok(RetireOutcome::NeverSuperseded) => {
            return api_error_with_details(
                ErrorStatus::Unprocessable,
                "this store is on its first lease and has never handed over to another machine, so                  there is no previous machine to retire",
                &[("generation", "0")],
            );
        }
        Ok(RetireOutcome::Raced) => {
            return api_error_with_details(
                ErrorStatus::FailedPrecondition,
                "this store's handover moved while you were retiring it; re-read and retry",
                &[("retired_at", "CHANGED")],
            );
        }
        Ok(RetireOutcome::NoLease) => {
            return not_found("store");
        }
        Err(error) => return lease_store_error_response(&error),
    }
    // `retired_by` is the admin's id here too, matching the row. The entry's own `actor` already
    // carries the email and the role as they stood, which is where a human-readable actor belongs.
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.lease.retire",
        "store",
        &request.store_id,
        None,
        Some(serde_json::json!({
            "retired_at": at.as_milliseconds_since_epoch(),
            "retired_by": context.admin.id,
        })),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}
