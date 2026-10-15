// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Operational alerts (`/admin/alerts`, ADR-0073): list, acknowledge and resolve.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use serde::Deserialize;

use crate::alerts::{AlertRecord, AlertStore, AlertStoreError};
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::Json;

// --- Operational alerts (`/admin/alerts`, ADR-0073, Track O2) -----------------------------------

#[derive(Clone)]
struct AlertState<Al, A, C> {
    alerts: Al,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// The console's view of a stored alert: the record with the kind/severity as their wire tokens and
/// the lifecycle instants as Unix ms.
#[derive(Debug, serde::Serialize)]
struct AlertView {
    id: String,
    tenant_id: Option<String>,
    kind: &'static str,
    dedup_key: String,
    severity: &'static str,
    summary: String,
    detail: serde_json::Value,
    first_seen_at_ms: i64,
    last_seen_at_ms: i64,
    resolved_at_ms: Option<i64>,
    acknowledged_at_ms: Option<i64>,
}

impl From<AlertRecord> for AlertView {
    fn from(record: AlertRecord) -> Self {
        Self {
            id: record.id,
            tenant_id: record.tenant_id.map(|tenant| tenant.to_string()),
            kind: record.kind.as_str(),
            dedup_key: record.dedup_key,
            severity: record.severity.as_str(),
            summary: record.summary,
            detail: record.detail,
            first_seen_at_ms: record.first_seen_at.as_milliseconds_since_epoch(),
            last_seen_at_ms: record.last_seen_at.as_milliseconds_since_epoch(),
            resolved_at_ms: record
                .resolved_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            acknowledged_at_ms: record
                .acknowledged_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
        }
    }
}

/// Whether the list returns recent history (active *and* resolved) rather than only the active alerts,
/// and how many rows at most.
#[derive(Debug, Clone, Deserialize)]
struct AlertListQuery {
    #[serde(default)]
    recent: bool,
    limit: Option<u32>,
}

/// The fleet-wide alert list, acknowledge, and resolve routes ([ADR-0073](../../../../docs/adr/0073-alerting.md)).
/// Reads are behind [`ConsolePermission::Read`] (every console role); acknowledge and resolve are behind
/// [`ConsolePermission::ManageAlerts`] (Owner/Admin/Ops) and are audited.
pub fn alerts_router<Al, A, C>(
    alerts: Al,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Al: AlertStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/alerts", get(admin_list_alerts::<Al, A, C>))
        .route("/admin/alerts/{id}/ack", post(admin_ack_alert::<Al, A, C>))
        .route(
            "/admin/alerts/{id}/resolve",
            post(admin_resolve_alert::<Al, A, C>),
        )
        .with_state(AlertState {
            alerts,
            admin,
            clock,
            audit,
        })
}

/// Maps an alert-store failure to a retryable `503`, logging the detail rather than leaking it.
fn alert_error_response(error: &AlertStoreError) -> Response {
    tracing::error!(%error, "an alert store operation failed");
    api_error(ErrorStatus::Unavailable, "the alert service is unavailable")
}

/// A super-admin (any console role) lists the fleet's alerts: the active set by default, or recent
/// history (active and resolved, newest-seen first, capped) with `?recent=true`.
async fn admin_list_alerts<Al, A, C>(
    State(state): State<AlertState<Al, A, C>>,
    headers: HeaderMap,
    Query(query): Query<AlertListQuery>,
) -> Response
where
    Al: AlertStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
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
    let result = if query.recent {
        state.alerts.list_recent(query.limit.unwrap_or(200)).await
    } else {
        state.alerts.list_active().await
    };
    match result {
        Ok(rows) => {
            let views: Vec<AlertView> = rows.into_iter().map(AlertView::from).collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(error) => alert_error_response(&error),
    }
}

/// A super-admin holding `console.alerts.manage` acknowledges an alert (idempotent).
async fn admin_ack_alert<Al, A, C>(
    State(state): State<AlertState<Al, A, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response
where
    Al: AlertStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageAlerts,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    match state.alerts.acknowledge(&id, state.clock.now()).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                None,
                "alert.acknowledge",
                "alert",
                &id,
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => alert_error_response(&error),
    }
}

/// A super-admin holding `console.alerts.manage` resolves an alert by hand (idempotent — a condition
/// still firing reopens on the next evaluator tick).
async fn admin_resolve_alert<Al, A, C>(
    State(state): State<AlertState<Al, A, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response
where
    Al: AlertStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageAlerts,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    match state.alerts.resolve(&id, state.clock.now()).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                None,
                "alert.resolve",
                "alert",
                &id,
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => alert_error_response(&error),
    }
}
