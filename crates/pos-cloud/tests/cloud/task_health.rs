// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Background-task health (`/admin/health/tasks`, ADR-0068 slice 4).

use axum::http::StatusCode;
use pos_cloud::{Cloud, health, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeRollups, FakeTaskHealth, FakeWebhooks, NOW_MS,
    admin_cookie, app_all, clock, get, get_with_cookie, json_body, provisioned_admin,
};

/// The main router (for `/admin/login`) merged with the health sub-router, one shared admin store.
fn health_app(admin: FakeAdmin, health: FakeTaskHealth, expected: Vec<String>) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::health_router(health, admin, clock(), expected))
}

#[tokio::test]
async fn task_health_reports_fresh_stale_and_never_ticked() {
    // The projector ticked 10s ago on a 30s interval (fresh); the dispatcher an hour ago (stale);
    // retention is expected but has never ticked (dead since boot).
    let store = FakeTaskHealth::default()
        .with_tick(
            health::ROLLUP_PROJECTOR,
            NOW_MS - 10_000,
            serde_json::json!({ "ok": true, "interval_secs": 30, "folded": 4 }),
        )
        .with_tick(
            health::WEBHOOK_DISPATCHER,
            NOW_MS - 3_600_000,
            serde_json::json!({ "ok": true, "interval_secs": 30 }),
        );
    let expected = vec![
        health::ROLLUP_PROJECTOR.to_owned(),
        health::WEBHOOK_DISPATCHER.to_owned(),
        health::RETENTION.to_owned(),
    ];
    let router = health_app(provisioned_admin(), store, expected);
    let cookie = admin_cookie(&router).await;

    let response = router
        .oneshot(get_with_cookie("/admin/health/tasks", &cookie))
        .await
        .expect("route the health read");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body["healthy"], false,
        "one expected loop is stale and one never ticked"
    );
    let tasks = body["tasks"].as_array().expect("a tasks array");
    assert_eq!(tasks.len(), 3);

    let projector = tasks
        .iter()
        .find(|task| task["task"] == health::ROLLUP_PROJECTOR)
        .expect("the projector is listed");
    assert_eq!(
        projector["healthy"], true,
        "a tick 10s ago within a 30s interval is fresh"
    );
    assert_eq!(projector["seconds_since"], 10);
    assert_eq!(
        projector["detail"]["folded"], 4,
        "the tick's detail is echoed"
    );

    let webhook = tasks
        .iter()
        .find(|task| task["task"] == health::WEBHOOK_DISPATCHER)
        .expect("the dispatcher is listed");
    assert_eq!(
        webhook["healthy"], false,
        "an hour-old tick is well past 30s times the slack"
    );

    let retention = tasks
        .iter()
        .find(|task| task["task"] == health::RETENTION)
        .expect("retention is listed even though it never ticked");
    assert_eq!(retention["expected"], true);
    assert_eq!(retention["healthy"], false, "never-ticked is never healthy");
    assert_eq!(retention["last_tick_at_ms"], serde_json::Value::Null);
}

#[tokio::test]
async fn task_health_flags_a_fresh_but_failing_loop() {
    // A loop that ticked recently but whose last tick's work failed is unhealthy — alive is not enough.
    let store = FakeTaskHealth::default().with_tick(
        health::ROLLUP_PROJECTOR,
        NOW_MS - 5_000,
        serde_json::json!({ "ok": false, "interval_secs": 30 }),
    );
    let router = health_app(
        provisioned_admin(),
        store,
        vec![health::ROLLUP_PROJECTOR.to_owned()],
    );
    let cookie = admin_cookie(&router).await;
    let body = json_body(
        router
            .oneshot(get_with_cookie("/admin/health/tasks", &cookie))
            .await
            .expect("route"),
    )
    .await;
    assert_eq!(
        body["healthy"], false,
        "a recent tick whose work failed is still unhealthy"
    );
    assert_eq!(body["tasks"][0]["healthy"], false);
}

#[tokio::test]
async fn task_health_needs_a_session() {
    let router = health_app(provisioned_admin(), FakeTaskHealth::default(), Vec::new());
    let denied = router
        .oneshot(get("/admin/health/tasks", None))
        .await
        .expect("route the health read");
    assert_eq!(
        denied.status(),
        StatusCode::UNAUTHORIZED,
        "the health view is behind the admin session guard"
    );
}
