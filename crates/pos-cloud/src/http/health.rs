// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Background-task health (`/admin/health/tasks`, ADR-0068 slice 4).

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::health::{TaskHealth, TaskHealthError, TaskHealthStore};
use crate::http::Json;

// --- Background-task health (`/admin/health/tasks`, ADR-0068 slice 4) ---------------------------

/// A task is judged **stale** if its most recent tick is older than its configured interval times
/// this slack — a few missed ticks, not one — so an interval-boundary jitter never flaps a healthy
/// loop to unhealthy. The interval comes from the tick's own recorded `interval_secs`.
const TASK_STALENESS_SLACK: i64 = 3;

/// The interval assumed for a tick whose detail omits `interval_secs` (a defensive default; every
/// loop this cloud ships records its interval).
const DEFAULT_TASK_INTERVAL_SECS: i64 = 300;

/// The collaborators the health route needs: the task-health store, the admin and clock the session
/// guard uses, and the set of task names this deployment *expects* to be running (so a loop that has
/// never ticked — dead since boot — is reported as unhealthy rather than silently absent). Its own
/// state, merged into the main router like [`FleetState`].
#[derive(Clone)]
struct HealthState<H, A, C> {
    health: H,
    admin: A,
    clock: C,
    expected: Vec<String>,
}

/// One background loop as the console sees it: identity, whether the deployment expects it to run, a
/// health verdict derived at read time, when it last ticked, and its self-describing detail.
#[derive(Debug, Clone, serde::Serialize)]
struct TaskHealthView {
    task: String,
    expected: bool,
    healthy: bool,
    last_tick_at_ms: Option<i64>,
    seconds_since: Option<i64>,
    detail: serde_json::Value,
}

impl TaskHealthView {
    /// The view for a task that has ticked at least once: fresh (within its interval × slack) and its
    /// last tick's work succeeded (`ok` — absent reads as alive).
    fn from_recorded(task: &str, expected: bool, recorded: &TaskHealth, now_ms: i64) -> Self {
        let last_tick_ms = recorded.last_tick_at.as_milliseconds_since_epoch();
        let interval_secs = recorded
            .detail
            .get("interval_secs")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(DEFAULT_TASK_INTERVAL_SECS);
        let ok = recorded
            .detail
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let age_secs = now_ms.saturating_sub(last_tick_ms) / 1000;
        let fresh = now_ms.saturating_sub(last_tick_ms)
            <= interval_secs
                .saturating_mul(1000)
                .saturating_mul(TASK_STALENESS_SLACK);
        Self {
            task: task.to_owned(),
            expected,
            healthy: fresh && ok,
            last_tick_at_ms: Some(last_tick_ms),
            seconds_since: Some(age_secs),
            detail: recorded.detail.clone(),
        }
    }

    /// The view for an expected task that has never ticked — dead since boot, so never healthy.
    fn never_ticked(task: &str) -> Self {
        Self {
            task: task.to_owned(),
            expected: true,
            healthy: false,
            last_tick_at_ms: None,
            seconds_since: None,
            detail: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// The whole-fleet-of-loops report: an overall verdict (every *expected* loop is healthy) plus the
/// per-loop views. Extra loops that ticked but the deployment does not expect are reported too
/// (`expected: false`), so nothing is hidden, but they do not sway the overall verdict.
#[derive(Debug, Clone, serde::Serialize)]
struct TaskHealthReport {
    healthy: bool,
    tasks: Vec<TaskHealthView>,
}

/// Builds the background-task health sub-router ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)
/// slice 4). One read, `GET /admin/health/tasks`, behind [`ConsolePermission::Read`] (every console
/// role). `expected` names the loops this deployment turned on, so a loop that never ticked is
/// surfaced as unhealthy rather than missing.
pub fn health_router<H, A, C>(health: H, admin: A, clock: C, expected: Vec<String>) -> Router
where
    H: TaskHealthStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/health/tasks", get(admin_task_health::<H, A, C>))
        .with_state(HealthState {
            health,
            admin,
            clock,
            expected,
        })
}

/// Maps a task-health read failure to a retryable `503`, logging the detail rather than leaking it.
fn health_error_response(error: &TaskHealthError) -> Response {
    tracing::error!(%error, "a task-health read failed");
    api_error(
        ErrorStatus::Unavailable,
        "the health service is unavailable",
    )
}

#[utoipa::path(
    get,
    path = "/admin/health/tasks",
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "`healthy`, whether every loop this deployment expects is \
                                      healthy, and `tasks`: each loop with `task`, `expected`, \
                                      `healthy` (it ticked within a few of its intervals and its last \
                                      tick succeeded), `last_tick_at_ms`, `seconds_since` and the \
                                      loop's own `detail`. An expected loop that never ticked is \
                                      listed as unhealthy; a loop that ticks but is not expected is \
                                      listed and does not count"),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The task-health store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "operations",
)]
/// Reports every background loop's health: each expected loop (whether or not it has ticked) plus any
/// extra loop that has. `503` if the store is unreachable.
async fn admin_task_health<H, A, C>(
    State(state): State<HealthState<H, A, C>>,
    headers: HeaderMap,
) -> Response
where
    H: TaskHealthStore + Clone + Send + Sync + 'static,
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
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    let recorded = match state.health.list_health().await {
        Ok(rows) => rows,
        Err(error) => return health_error_response(&error),
    };

    let mut tasks = Vec::new();
    // Every expected loop first, in the deployment's order: its recorded row if it has ticked, else a
    // never-ticked (unhealthy) placeholder.
    for task in &state.expected {
        match recorded.iter().find(|row| &row.task == task) {
            Some(row) => tasks.push(TaskHealthView::from_recorded(task, true, row, now_ms)),
            None => tasks.push(TaskHealthView::never_ticked(task)),
        }
    }
    // Then any loop that ticked but the deployment does not expect — surfaced, but not counted toward
    // the overall verdict.
    for row in &recorded {
        if !state.expected.iter().any(|task| task == &row.task) {
            tasks.push(TaskHealthView::from_recorded(&row.task, false, row, now_ms));
        }
    }

    let healthy = tasks
        .iter()
        .filter(|view| view.expected)
        .all(|view| view.healthy);
    (StatusCode::OK, Json(TaskHealthReport { healthy, tasks })).into_response()
}
