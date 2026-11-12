// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `package` settings (ADR-0189 decision 2): a pin names published versions only, a lock at a
//! broad scope cannot be loosened below it, and a store's choice is written only through its route,
//! only from its allowed versions.
//!
//! The layout is the settings tests': the brand holds the first and second stores, the third has
//! no brand, and the group "North" holds the first and the third.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud_config::package_pin::InMemoryReleases;
use pos_cloud_config::packages::{InMemoryPackages, PackageStore, PackageVersionRecord};
use pos_proto::ids::{PackageId, PackageVersionId, StoreId};
use pos_proto::package::PackageContent;
use pos_proto::text::ReleaseTag;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, admin_cookie, json_body, put_with_cookie, settings_app_with_packages,
    settings_brand, settings_stores, tenant, tenant_layer_node,
};

const V1: &str = "01J00000000000000000000V01";
const V2: &str = "01J00000000000000000000V02";
const UNPUBLISHED: &str = "01J00000000000000000000V09";

/// A package store holding versions `V1` and `V2` of one package of the tenant.
async fn packages() -> InMemoryPackages {
    let store = InMemoryPackages::new();
    for id in [V1, V2] {
        let version = PackageVersionRecord {
            content: PackageContent {
                package_id: PackageId::new(Ulid::from_u128(0xFAC)),
                package_version_id: id.parse::<PackageVersionId>().expect("an id"),
                version_label: id.to_owned(),
                min_edge_version: ReleaseTag::new("0.14.1"),
                time_slots: Vec::new(),
                flows: Vec::new(),
                screens: Vec::new(),
                data: pos_proto::package::PackageData::default(),
                reactions: Vec::new(),
                home_screens: Vec::new(),
            },
            create_time: Timestamp::EPOCH,
            tap_warning_count: 0,
        };
        store
            .publish_version(tenant(), &version)
            .await
            .expect("publish");
    }
    store
}

async fn app() -> (axum::Router, FakeConfigTrees, String) {
    let (router, config_trees, _) = settings_app_with_packages(
        Arc::new(NoopAuditRecorder),
        packages().await,
        InMemoryReleases::new(),
    );
    let cookie = admin_cookie(&router).await;
    (router, config_trees, cookie)
}

fn setting(key: &str, scope: &str, scope_id: &str, value: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "setting_key": key,
        "scope": scope,
        "scope_id": scope_id,
    });
    body["value"] = value;
    body
}

async fn put(
    router: &axum::Router,
    uri: &str,
    body: &serde_json::Value,
    cookie: &str,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(put_with_cookie(uri, body, cookie))
        .await
        .expect("route");
    let status = response.status();
    (status, json_body(response).await)
}

fn choice(store: StoreId, version: Option<&str>) -> (String, serde_json::Value) {
    (
        format!("/admin/stores/{store}/package/choice"),
        serde_json::json!({
            "tenant_id": tenant().as_ulid().to_string(),
            "package_version_id": version,
        }),
    )
}

fn tenant_scope() -> String {
    tenant().as_ulid().to_string()
}

/// A lock and a choice name published versions; an id that names none is refused, and nothing is
/// written.
#[tokio::test]
async fn a_pin_names_published_versions_only() {
    let (router, config_trees, cookie) = app().await;
    let [first, ..] = settings_stores();

    let (status, body) = put(
        &router,
        "/admin/settings",
        &setting(
            "package.allowed_version_ids",
            "SETTING_SCOPE_TENANT",
            &tenant_scope(),
            serde_json::json!([V1, UNPUBLISHED]),
        ),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("UNKNOWN_PACKAGE_VERSION"),
        "{body}"
    );
    assert_eq!(
        tenant_layer_node(&config_trees, first, "package").await,
        None
    );

    let (status, body) = put(
        &router,
        "/admin/settings",
        &setting(
            "package.locked_version_id",
            "SETTING_SCOPE_TENANT",
            &tenant_scope(),
            serde_json::json!(V1),
        ),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        tenant_layer_node(&config_trees, first, "package").await,
        Some(serde_json::json!({ "locked_version_id": V1 }))
    );
}

/// A brand's lock refuses a narrower `package` value for its stores, and only for them; the scope
/// that holds the lock may still change it.
#[tokio::test]
async fn a_brand_lock_cannot_be_loosened_below_it() {
    let (router, config_trees, cookie) = app().await;
    let [first, _, third] = settings_stores();
    let brand = settings_brand().to_string();

    let (status, _) = put(
        &router,
        "/admin/settings",
        &setting(
            "package.mode",
            "SETTING_SCOPE_BRAND",
            &brand,
            serde_json::json!("PACKAGE_MODE_LOCKED"),
        ),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let loosen = |store: StoreId| {
        setting(
            "package.mode",
            "SETTING_SCOPE_STORE",
            &store.to_string(),
            serde_json::json!("PACKAGE_MODE_CHOOSE"),
        )
    };
    let (status, body) = put(&router, "/admin/settings", &loosen(first), &cookie).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.to_string().contains("PACKAGE_LOCKED_ABOVE"), "{body}");
    assert_eq!(
        tenant_layer_node(&config_trees, first, "package").await,
        Some(serde_json::json!({ "mode": "PACKAGE_MODE_LOCKED" })),
        "the brand's lock still reaches the store"
    );

    let (status, _) = put(&router, "/admin/settings", &loosen(third), &cookie).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the third store has no brand, so no lock"
    );

    let (status, _) = put(
        &router,
        "/admin/settings",
        &setting(
            "package.mode",
            "SETTING_SCOPE_BRAND",
            &brand,
            serde_json::json!("PACKAGE_MODE_CHOOSE"),
        ),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the lock's own scope changes it");
    let (status, _) = put(&router, "/admin/settings", &loosen(first), &cookie).await;
    assert_eq!(status, StatusCode::OK, "lifted, the store may set its own");
}

/// A store chooses among its allowed versions through its route, and only there: not a version it
/// is not allowed, not while locked, and never through `PUT /admin/settings`. Clearing the choice
/// returns it to the default version.
#[tokio::test]
async fn a_store_chooses_an_allowed_version_through_its_route() {
    let (router, config_trees, cookie) = app().await;
    let [first, second, _] = settings_stores();
    for (key, value) in [
        ("package.mode", serde_json::json!("PACKAGE_MODE_CHOOSE")),
        ("package.allowed_version_ids", serde_json::json!([V1, V2])),
        ("package.default_version_id", serde_json::json!(V1)),
    ] {
        let (status, body) = put(
            &router,
            "/admin/settings",
            &setting(key, "SETTING_SCOPE_TENANT", &tenant_scope(), value),
            &cookie,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{key}: {body}");
    }

    let (status, body) = put(
        &router,
        "/admin/settings",
        &setting(
            "package.chosen_version_id",
            "SETTING_SCOPE_STORE",
            &first.to_string(),
            serde_json::json!(V2),
        ),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("WRITTEN_THROUGH_ITS_ROUTE"),
        "{body}"
    );

    let (uri, body) = choice(first, Some(V2));
    let (status, answer) = put(&router, &uri, &body, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let node = tenant_layer_node(&config_trees, first, "package")
        .await
        .expect("a package node");
    assert_eq!(node["chosen_version_id"], V2);
    assert_eq!(
        tenant_layer_node(&config_trees, second, "package")
            .await
            .expect("a node")["chosen_version_id"],
        serde_json::Value::Null,
        "the choice is the first store's alone"
    );

    let (uri, body) = choice(first, Some(UNPUBLISHED));
    let (status, answer) = put(&router, &uri, &body, &cookie).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert!(answer.to_string().contains("NOT_ALLOWED"), "{answer}");

    let (uri, body) = choice(first, None);
    let (status, _) = put(&router, &uri, &body, &cookie).await;
    assert_eq!(status, StatusCode::OK);
    let node = tenant_layer_node(&config_trees, first, "package")
        .await
        .expect("a package node");
    assert_eq!(node.get("chosen_version_id"), None, "the choice is cleared");

    let (status, _) = put(
        &router,
        "/admin/settings",
        &setting(
            "package.mode",
            "SETTING_SCOPE_TENANT",
            &tenant_scope(),
            serde_json::json!("PACKAGE_MODE_LOCKED"),
        ),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (uri, body) = choice(first, Some(V2));
    let (status, answer) = put(&router, &uri, &body, &cookie).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert!(
        answer.to_string().contains("PACKAGE_NOT_CHOOSING"),
        "{answer}"
    );
}
