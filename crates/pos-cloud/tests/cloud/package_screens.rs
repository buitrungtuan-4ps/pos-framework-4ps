// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A version's screens (ADR-0190, ADR-0203) are published and checked, and a publish's dry run
//! (ADR-0190 decision 8) answers what the publish would, says where a fault is, and stores nothing.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::auth::admin::AdminRole;

use super::{package_app, send, version_body};
use crate::{
    admin_cookie, get_with_cookie, post_with_cookie, provisioned_admin, role_session_cookie,
    ulid_text,
};

/// A screen: a frame holding a heading and a button that opens another screen.
fn screens(heading: &serde_json::Value) -> serde_json::Value {
    serde_json::json!([{
        "body": {
            "screen_id": "welcome",
            "root": {
                "component_id": "screen_frame",
                "children": [
                    heading,
                    {
                        "component_id": "action_button",
                        "properties": {
                            "label_key": "welcome.start",
                            "action": { "action_id": "ui.screen.open", "params": { "screen_id": "floor" } }
                        }
                    }
                ]
            }
        }
    }])
}

fn screens_body(tenant: &str, heading: &serde_json::Value) -> serde_json::Value {
    let mut body = version_body(tenant, "March", "[]");
    body["screens"] = screens(heading);
    body
}

/// A dry run of a draft whose heading lacks its title names the screen, the node and the property;
/// the same draft is refused by a publish; once fixed, the dry run accepts it, still stores
/// nothing, and the publish stores the screens it checked.
#[tokio::test]
async fn a_dry_run_says_where_a_screen_is_refused_and_stores_nothing() {
    let router = package_app(provisioned_admin(), Arc::new(NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let (_, package) = send(
        &router,
        post_with_cookie(
            "/admin/packages",
            &serde_json::json!({ "tenant_id": tenant, "name": "Counter" }),
            &cookie,
        ),
    )
    .await;
    let versions = format!(
        "/admin/packages/{}/versions",
        package["package_id"].as_str().expect("an id")
    );
    let dry_run = format!("{versions}/dry-run");

    let untitled = screens_body(
        &tenant,
        &serde_json::json!({ "component_id": "page_header" }),
    );
    let (status, verdict) = send(&router, post_with_cookie(&dry_run, &untitled, &cookie)).await;
    assert_eq!(status, StatusCode::OK, "{verdict}");
    assert_eq!(verdict["accepted"], false);
    let refusal = &verdict["refusals"][0];
    assert_eq!(refusal["field"], "screens");
    assert_eq!(refusal["reason"], "MISSING_PROPERTY");
    assert_eq!(refusal["screen_index"], 0);
    assert_eq!(refusal["node_index"], 1, "the heading, after the frame");
    assert_eq!(refusal["name"], "title_key");
    let (status, body) = send(&router, post_with_cookie(&versions, &untitled, &cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("screens"), "{body}");

    let titled = screens_body(
        &tenant,
        &serde_json::json!({ "component_id": "page_header", "properties": { "title_key": "welcome.title" } }),
    );
    let (status, verdict) = send(&router, post_with_cookie(&dry_run, &titled, &cookie)).await;
    assert_eq!(status, StatusCode::OK, "{verdict}");
    assert_eq!(verdict["accepted"], true, "{verdict}");
    assert_eq!(verdict["refusals"], serde_json::json!([]));
    assert_eq!(verdict["min_edge_version"], "0.14.1");
    let list = format!("{versions}?tenant_id={tenant}");
    let (_, listed) = send(&router, get_with_cookie(&list, &cookie)).await;
    assert_eq!(listed, serde_json::json!([]), "a dry run stores nothing");

    let (status, published) = send(&router, post_with_cookie(&versions, &titled, &cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(
        published["content"]["screens"],
        screens(&titled["screens"][0]["body"]["root"]["children"][0])
    );
}

/// A dry run is an author's act: a viewer is refused it, and a package the tenant lacks is not
/// found.
#[tokio::test]
async fn a_dry_run_is_for_authors_of_a_package_the_tenant_has() {
    let admin = provisioned_admin();
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let router = package_app(admin, Arc::new(NoopAuditRecorder));
    let tenant = ulid_text(1);
    let dry_run = format!("/admin/packages/{}/versions/dry-run", ulid_text(9));
    let body = version_body(&tenant, "March", "[]");

    let (status, _) = send(&router, post_with_cookie(&dry_run, &body, &viewer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let router = package_app(provisioned_admin(), Arc::new(NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let (status, _) = send(&router, post_with_cookie(&dry_run, &body, &cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Every key a version names needs an English text in the tenant's grid (ADR-0190 decision 6): a
/// heading whose key has only a Vietnamese text and a blank English one is refused where it is,
/// with the key named, by the dry run and by the publish; a time slot whose key the grid lacks is
/// refused on its field; and the same draft passes once every key has English.
#[tokio::test]
async fn a_key_without_english_is_refused_where_the_version_names_it() {
    let router = package_app(provisioned_admin(), Arc::new(NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let (_, package) = send(
        &router,
        post_with_cookie(
            "/admin/packages",
            &serde_json::json!({ "tenant_id": tenant, "name": "Counter" }),
            &cookie,
        ),
    )
    .await;
    let versions = format!(
        "/admin/packages/{}/versions",
        package["package_id"].as_str().expect("an id")
    );
    let dry_run = format!("{versions}/dry-run");

    let untranslated = screens_body(
        &tenant,
        &serde_json::json!({ "component_id": "page_header", "properties": { "title_key": "welcome.untranslated" } }),
    );
    let (status, verdict) = send(&router, post_with_cookie(&dry_run, &untranslated, &cookie)).await;
    assert_eq!(status, StatusCode::OK, "{verdict}");
    assert_eq!(verdict["accepted"], false);
    let refusal = &verdict["refusals"][0];
    assert_eq!(refusal["field"], "screens");
    assert_eq!(refusal["reason"], "UNKNOWN_TRANSLATION_KEY");
    assert_eq!(refusal["screen_index"], 0);
    assert_eq!(refusal["node_index"], 1, "the heading, after the frame");
    assert_eq!(refusal["name"], "welcome.untranslated");
    let (status, body) = send(&router, post_with_cookie(&versions, &untranslated, &cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("UNKNOWN_TRANSLATION_KEY"),
        "{body}"
    );

    let mut dinner = screens_body(
        &tenant,
        &serde_json::json!({ "component_id": "page_header", "properties": { "title_key": "welcome.title" } }),
    );
    dinner["time_slots"] = serde_json::json!([{
        "time_slot_id": "dinner",
        "label_key": "package.slot.dinner",
        "schedules": [{ "days": 31, "start_minute": 1080, "end_minute": 1320 }]
    }]);
    let (_, verdict) = send(&router, post_with_cookie(&dry_run, &dinner, &cookie)).await;
    let refusal = &verdict["refusals"][0];
    assert_eq!(refusal["field"], "time_slots.label_key", "{verdict}");
    assert_eq!(refusal["reason"], "UNKNOWN_TRANSLATION_KEY");
    assert_eq!(refusal["name"], "package.slot.dinner");
    assert!(refusal.get("screen_index").is_none(), "{verdict}");

    dinner["time_slots"][0]["label_key"] = serde_json::json!("package.slot.lunch");
    let (_, verdict) = send(&router, post_with_cookie(&dry_run, &dinner, &cookie)).await;
    assert_eq!(verdict["accepted"], true, "{verdict}");
    let (status, body) = send(&router, post_with_cookie(&versions, &dinner, &cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// A version names its homes (ADR-0210 decision 1): a home that names a screen the version does
/// not declare is refused where it is, by the dry run and by the publish; a base home naming a
/// declared screen publishes, and the version keeps it.
#[tokio::test]
async fn a_version_names_the_screen_each_role_starts_on() {
    let router = package_app(provisioned_admin(), Arc::new(NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let (_, package) = send(
        &router,
        post_with_cookie(
            "/admin/packages",
            &serde_json::json!({ "tenant_id": tenant, "name": "Counter" }),
            &cookie,
        ),
    )
    .await;
    let versions = format!(
        "/admin/packages/{}/versions",
        package["package_id"].as_str().expect("an id")
    );
    let dry_run = format!("{versions}/dry-run");
    let mut body = screens_body(
        &tenant,
        &serde_json::json!({ "component_id": "page_header", "properties": { "title_key": "welcome.title" } }),
    );

    body["home_screens"] = serde_json::json!([{ "body": { "screen_id": "floor" } }]);
    let (status, verdict) = send(&router, post_with_cookie(&dry_run, &body, &cookie)).await;
    assert_eq!(status, StatusCode::OK, "{verdict}");
    let refusal = &verdict["refusals"][0];
    assert_eq!(refusal["field"], "home_screens", "{verdict}");
    assert_eq!(refusal["reason"], "UNKNOWN_HOME_SCREEN");
    assert_eq!(refusal["name"], "0");
    let (status, refused) = send(&router, post_with_cookie(&versions, &body, &cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");

    body["home_screens"] = serde_json::json!([{ "body": { "screen_id": "welcome" } }]);
    let (status, verdict) = send(&router, post_with_cookie(&dry_run, &body, &cookie)).await;
    assert_eq!(status, StatusCode::OK, "{verdict}");
    assert_eq!(verdict["accepted"], true, "{verdict}");
    let (status, published) = send(&router, post_with_cookie(&versions, &body, &cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(published["content"]["home_screens"], body["home_screens"]);
}
