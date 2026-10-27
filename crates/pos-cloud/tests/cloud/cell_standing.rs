// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store whose country differs from its cell's (`/admin/stores/{id}/cell-standing` and
//! `/cell-acknowledgement`): a warning and an acknowledgement recorded in the audit trail, never a
//! refusal (ADR-0179 C6).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::cell::Cell;
use pos_cloud::http;
use pos_proto::locale::CountryCode;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, admin_cookie, app_all, clock, get_with_cookie, json_body,
    post_with_cookie, provisioned_admin, store_id, tenant,
};

const ACTION: &str = "store.cell_country.acknowledge";

fn vn_cell() -> Cell {
    Cell {
        cell_id: "vn1".to_owned(),
        country_alias: Some("vn".to_owned()),
        home_country: Some(CountryCode::VN),
        base_url: Some("https://vn1.example.com".to_owned()),
        runs_in: Some(CountryCode::VN),
        configured: true,
    }
}

fn standing_app(trees: FakeConfigTrees, audit: FakeAudit, cell: Cell) -> axum::Router {
    let admin = provisioned_admin();
    let app = app_all(
        pos_cloud::Cloud::new(pos_fakes::FakeStore::new()),
        crate::FakeRollups::default(),
        crate::FakeKeys::default(),
        admin.clone(),
        trees.clone(),
        crate::FakeWebhooks::default(),
    );
    http::router(app).merge(http::cell_standing_router(
        audit,
        trees,
        admin,
        clock(),
        Arc::new(cell),
    ))
}

fn trees_publishing(country: &str) -> FakeConfigTrees {
    let trees = FakeConfigTrees::default();
    trees.seed_store_layer(
        tenant(),
        store_id(),
        serde_json::json!({ "locale": { "country_code": country } }),
    );
    trees
}

fn standing_uri() -> String {
    format!(
        "/admin/stores/{}/cell-standing?tenant_id={}",
        store_id(),
        tenant().as_ulid()
    )
}

fn acknowledgement_uri() -> String {
    format!("/admin/stores/{}/cell-acknowledgement", store_id())
}

fn answer(profile: &str, cell: &str, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "profile_country": profile,
        "cell_country": cell,
        "reason": reason,
    })
}

#[tokio::test]
async fn a_difference_is_warned_answered_and_audited() {
    let audit = FakeAudit::default();
    let router = standing_app(trees_publishing("JP"), audit.clone(), vn_cell());
    let cookie = admin_cookie(&router).await;

    let before = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(&standing_uri(), &cookie))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(before["differs"], true);
    assert_eq!(before["cell_country"], "VN");
    assert_eq!(before["profile_country"], "JP");
    assert_eq!(before["acknowledgement"], serde_json::Value::Null);

    let answered = router
        .clone()
        .oneshot(post_with_cookie(
            &acknowledgement_uri(),
            &answer("JP", "VN", "  Group DTA covers JP stores hosted in VN. "),
            &cookie,
        ))
        .await
        .expect("route the answer");
    assert_eq!(answered.status(), StatusCode::OK);
    let recorded = json_body(answered).await;
    assert_eq!(
        recorded["reason"],
        "Group DTA covers JP stores hosted in VN."
    );

    // A later, unrelated entry about the same store does not hide the answer.
    {
        let mut entries = audit.entries.lock().expect("lock");
        let mut other = entries.last().expect("the answer").clone();
        other.action = "store.update".to_owned();
        other.after = Some(serde_json::json!({ "display_name": "Renamed" }));
        entries.push(other);
    }

    let after = json_body(
        router
            .oneshot(get_with_cookie(&standing_uri(), &cookie))
            .await
            .expect("route the read again"),
    )
    .await;
    assert_eq!(
        after["differs"], true,
        "an answer explains, it does not erase"
    );
    assert_eq!(
        after["acknowledgement"]["reason"],
        "Group DTA covers JP stores hosted in VN."
    );

    let entries = audit.entries.lock().expect("lock").clone();
    let entry = entries
        .iter()
        .find(|entry| entry.action == ACTION)
        .expect("the answer is in the trail");
    assert_eq!(entry.entity_type, "store");
    assert_eq!(entry.entity_id, store_id().to_string());
    assert_eq!(entry.tenant_id, Some(tenant()));
    let detail = entry.after.as_ref().expect("the detail");
    assert_eq!(detail["profile_country"], "JP");
    assert_eq!(detail["cell_country"], "VN");
}

#[tokio::test]
async fn an_answer_stops_standing_when_the_store_moves() {
    let audit = FakeAudit::default();
    let router = standing_app(trees_publishing("JP"), audit.clone(), vn_cell());
    let cookie = admin_cookie(&router).await;
    let answered = router
        .clone()
        .oneshot(post_with_cookie(
            &acknowledgement_uri(),
            &answer("JP", "VN", "Covered."),
            &cookie,
        ))
        .await
        .expect("route the answer");
    assert_eq!(answered.status(), StatusCode::OK);

    // The same trail, a store now publishing another country.
    let moved = standing_app(trees_publishing("IN"), audit, vn_cell());
    let cookie = admin_cookie(&moved).await;
    let read = json_body(
        moved
            .oneshot(get_with_cookie(&standing_uri(), &cookie))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(read["differs"], true);
    assert_eq!(
        read["acknowledgement"],
        serde_json::Value::Null,
        "a reason about JP says nothing about IN"
    );
}

#[tokio::test]
async fn a_store_in_its_cells_country_has_nothing_to_answer() {
    let router = standing_app(trees_publishing("VN"), FakeAudit::default(), vn_cell());
    let cookie = admin_cookie(&router).await;
    let read = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(&standing_uri(), &cookie))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(read["differs"], false);

    let refused = router
        .oneshot(post_with_cookie(
            &acknowledgement_uri(),
            &answer("VN", "VN", "Nothing to say."),
            &cookie,
        ))
        .await
        .expect("route the answer");
    assert_eq!(refused.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_stale_pair_or_a_blank_reason_is_refused_and_nothing_recorded() {
    let audit = FakeAudit::default();
    let router = standing_app(trees_publishing("JP"), audit.clone(), vn_cell());
    let cookie = admin_cookie(&router).await;

    let stale = router
        .clone()
        .oneshot(post_with_cookie(
            &acknowledgement_uri(),
            &answer("IN", "VN", "About another country."),
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let blank = router
        .clone()
        .oneshot(post_with_cookie(
            &acknowledgement_uri(),
            &answer("JP", "VN", "   "),
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(blank).await["error"]["details"][0]["reason"],
        "REQUIRED"
    );

    let malformed = router
        .oneshot(post_with_cookie(
            &acknowledgement_uri(),
            &answer("Japan", "VN", "Covered."),
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    assert!(
        audit
            .entries
            .lock()
            .expect("lock")
            .iter()
            .all(|entry| entry.action != ACTION),
        "a refused answer records nothing"
    );
}

#[tokio::test]
async fn a_single_default_cell_compares_with_nothing() {
    let router = standing_app(trees_publishing("JP"), FakeAudit::default(), Cell::single());
    let cookie = admin_cookie(&router).await;
    let read = json_body(
        router
            .oneshot(get_with_cookie(&standing_uri(), &cookie))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(read["differs"], false);
    assert_eq!(read["cell_id"], "default");
    assert_eq!(read["cell_country"], serde_json::Value::Null);
}
