// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The capability catalogue (`/admin/capabilities`): its flags, presets and rules.

use axum::http::StatusCode;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, app_all, clock,
    get_with_cookie, json_body, provisioned_admin, role_session_cookie,
};

/// The main app merged with the capability-catalogue router.
fn capabilities_app(admin: FakeAdmin) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::capabilities_router(admin, clock()))
}

#[tokio::test]
async fn capability_catalogue_serves_flags_presets_and_rules_to_any_admin() {
    let admin = provisioned_admin();
    let router = capabilities_app(admin.clone());

    // Read is open to every console role — a Viewer may load the catalogue.
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let response = router
        .clone()
        .oneshot(get_with_cookie("/admin/capabilities", &viewer))
        .await
        .expect("route the catalogue");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    // Flags carry key/default/description; the well-known ones are present.
    let flags = body["flags"].as_array().expect("flags array");
    assert!(!flags.is_empty());
    let keys: Vec<&str> = flags
        .iter()
        .map(|flag| flag["key"].as_str().expect("a key"))
        .collect();
    assert!(keys.contains(&"tables_enabled"));
    assert!(keys.contains(&"pay_first_enabled"));
    let tables = flags
        .iter()
        .find(|flag| flag["key"] == serde_json::json!("tables_enabled"))
        .expect("the tables flag");
    assert_eq!(tables["default_on"], serde_json::json!(true));
    assert!(tables["description"].as_str().expect("a description").len() > 5);

    // The three presets are offered, each as a set of keys.
    let presets = body["presets"].as_array().expect("presets array");
    let preset_ids: Vec<&str> = presets
        .iter()
        .map(|preset| preset["id"].as_str().expect("an id"))
        .collect();
    assert!(preset_ids.contains(&"full_service"));
    assert!(preset_ids.contains(&"counter"));
    assert!(preset_ids.contains(&"retail"));

    // The inter-flag rules are served for the console's conflict preview.
    let rules = body["rules"].as_array().expect("rules array");
    let rule_ids: Vec<&str> = rules
        .iter()
        .map(|rule| rule["id"].as_str().expect("an id"))
        .collect();
    assert!(rule_ids.contains(&"pay_first.excludes.tables"));
    assert!(rule_ids.contains(&"seats.requires.tables"));
}

/// The console offers a switch only where a release reads it (ADR-0160 decision 5): tabs, pay-first,
/// barcode entry and queue numbers stay in the catalogue, marked not offered, and the presets that
/// turn them on still name them; every other flag is offered.
#[tokio::test]
async fn the_catalogue_offers_only_the_switches_a_release_reads() {
    let admin = provisioned_admin();
    let router = capabilities_app(admin.clone());
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let body = json_body(
        router
            .oneshot(get_with_cookie("/admin/capabilities", &viewer))
            .await
            .expect("route the catalogue"),
    )
    .await;

    let offered: Vec<(&str, bool)> = body["flags"]
        .as_array()
        .expect("flags array")
        .iter()
        .map(|flag| {
            (
                flag["key"].as_str().expect("a key"),
                flag["offered"].as_bool().expect("offered is a boolean"),
            )
        })
        .collect();
    assert_eq!(
        offered,
        vec![
            ("tables_enabled", true),
            ("tabs_enabled", false),
            ("seats_enabled", true),
            ("kds_enabled", true),
            ("courses_enabled", true),
            ("pay_first_enabled", false),
            ("barcode_enabled", false),
            ("queue_number_enabled", false),
            ("tips_enabled", true),
            ("qr_ordering_enabled", true),
        ]
    );
    let counter = body["presets"]
        .as_array()
        .expect("presets array")
        .iter()
        .find(|preset| preset["id"] == "counter")
        .expect("the counter preset");
    assert!(
        counter["keys"]
            .as_array()
            .expect("keys")
            .contains(&serde_json::json!("pay_first_enabled")),
        "the preset is unchanged: {counter}"
    );
}
