// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The pinned package version rides the store's document (ADR-0189 decisions 3, 5 and 7): it is
//! compiled in for a store whose reported release meets it, a store too old for it or of unknown
//! release keeps what it runs and shows as waiting, pinning the previous version rolls back, and no
//! pin leaves the default package.
//!
//! The layout is the settings tests': the brand holds the first and second stores, the third has
//! no brand.

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
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, admin_cookie, get_with_cookie, json_body, post_with_cookie, put_with_cookie,
    settings_app_with_packages, settings_stores, tenant, tenant_layer_node,
};

const V1: &str = "01J00000000000000000000V01";
const V2: &str = "01J00000000000000000000V02";

struct App {
    router: axum::Router,
    config_trees: FakeConfigTrees,
    releases: InMemoryReleases,
    cookie: String,
}

/// The settings routes with versions `V1` and `V2` published, each needing 0.14.1.
async fn app() -> App {
    let packages = InMemoryPackages::new();
    for id in [V1, V2] {
        let version = PackageVersionRecord {
            content: PackageContent {
                package_id: PackageId::new(Ulid::from_u128(0xFAC)),
                package_version_id: id.parse::<PackageVersionId>().expect("an id"),
                version_label: id.to_owned(),
                min_edge_version: ReleaseTag::new("0.14.1"),
                time_slots: Vec::new(),
                flows: Vec::new(),
            },
            create_time: Timestamp::EPOCH,
        };
        packages
            .publish_version(tenant(), &version)
            .await
            .expect("publish");
    }
    let releases = InMemoryReleases::new();
    let (router, config_trees, _) =
        settings_app_with_packages(Arc::new(NoopAuditRecorder), packages, releases.clone());
    let cookie = admin_cookie(&router).await;
    App {
        router,
        config_trees,
        releases,
        cookie,
    }
}

impl App {
    async fn send(&self, request: axum::http::Request<axum::body::Body>) -> (StatusCode, Value) {
        let response = self.router.clone().oneshot(request).await.expect("route");
        let status = response.status();
        (status, json_body(response).await)
    }

    /// Writes one tenant-scope `package` setting, which republishes every store.
    async fn pin(&self, field: &str, value: Value) {
        let mut body = json!({
            "tenant_id": tenant().as_ulid().to_string(),
            "setting_key": format!("package.{field}"),
            "scope": "SETTING_SCOPE_TENANT",
            "scope_id": tenant().as_ulid().to_string(),
        });
        body["value"] = value;
        let (status, answer) = self
            .send(put_with_cookie("/admin/settings", &body, &self.cookie))
            .await;
        assert_eq!(status, StatusCode::OK, "{field}: {answer}");
    }

    /// The version the store's document carries, if any.
    async fn running(&self, store: StoreId) -> Option<String> {
        tenant_layer_node(&self.config_trees, store, "package")
            .await
            .and_then(|node| {
                node["content"]["package_version_id"]
                    .as_str()
                    .map(str::to_owned)
            })
    }

    async fn status(&self, store: StoreId) -> Value {
        let (status, body) = self
            .send(get_with_cookie(
                &format!(
                    "/admin/stores/{store}/package?tenant_id={}",
                    tenant().as_ulid()
                ),
                &self.cookie,
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }
}

/// A lock reaches the document of a store whose release meets the version; a store too old, and
/// one that never reported, keep what they run and show why. A republish once the old store has
/// upgraded brings it level.
#[tokio::test]
async fn a_pinned_version_rides_the_document_of_a_store_whose_release_meets_it() {
    let app = app().await;
    let [first, second, third] = settings_stores();
    app.releases.report(tenant(), first, "0.14.1");
    app.releases.report(tenant(), second, "v0.13.9");

    app.pin("locked_version_id", json!(V1)).await;
    assert_eq!(app.running(first).await, None, "no mode, no pin");
    app.pin("mode", json!("PACKAGE_MODE_LOCKED")).await;

    assert_eq!(app.running(first).await.as_deref(), Some(V1));
    assert_eq!(
        app.status(first).await,
        json!({ "pinned_version_id": V1, "running_version_id": V1 })
    );
    assert_eq!(
        app.running(second).await,
        None,
        "too old: it keeps what it runs"
    );
    assert_eq!(
        app.status(second).await,
        json!({
            "pinned_version_id": V1,
            "waiting": { "reason": "EDGE_TOO_OLD", "installed_release": "v0.13.9" },
        })
    );
    assert_eq!(
        app.status(third).await["waiting"]["reason"],
        "RELEASE_UNKNOWN"
    );

    app.releases.report(tenant(), second, "0.14.1");
    let (status, _) = app
        .send(post_with_cookie(
            "/admin/settings/publish",
            &json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": second.to_string(),
            }),
            &app.cookie,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.running(second).await.as_deref(), Some(V1));
}

/// A store too old for a new version keeps the previous one it runs, rather than losing it.
/// Pinning the previous version is the rollback, and no pin returns the store to the default
/// package.
#[tokio::test]
async fn a_store_keeps_its_version_while_waiting_and_rolls_back_by_pinning_the_previous_one() {
    let app = app().await;
    let [first, ..] = settings_stores();
    app.releases.report(tenant(), first, "0.14.1");
    app.pin("mode", json!("PACKAGE_MODE_LOCKED")).await;
    app.pin("locked_version_id", json!(V1)).await;
    assert_eq!(app.running(first).await.as_deref(), Some(V1));

    app.pin("locked_version_id", json!(V2)).await;
    assert_eq!(app.running(first).await.as_deref(), Some(V2));

    app.releases.report(tenant(), first, "0.14.0");
    app.pin("locked_version_id", json!(V1)).await;
    assert_eq!(
        app.running(first).await.as_deref(),
        Some(V2),
        "a store that went back to an older release keeps the version it runs"
    );

    app.releases.report(tenant(), first, "0.14.1");
    app.pin("locked_version_id", json!(V1)).await;
    assert_eq!(app.running(first).await.as_deref(), Some(V1), "rolled back");

    app.pin("mode", json!("PACKAGE_MODE_UNSPECIFIED")).await;
    assert_eq!(
        app.running(first).await,
        None,
        "unpinned: the default package"
    );
    assert_eq!(app.status(first).await, json!({}));
}

/// A choice runs the version the store chose, and its default until it chooses.
#[tokio::test]
async fn a_choosing_store_runs_its_choice_or_its_default() {
    let app = app().await;
    let [first, second, _] = settings_stores();
    for store in [first, second] {
        app.releases.report(tenant(), store, "0.14.1");
    }
    app.pin("allowed_version_ids", json!([V1, V2])).await;
    app.pin("default_version_id", json!(V1)).await;
    app.pin("mode", json!("PACKAGE_MODE_CHOOSE")).await;
    let (status, _) = app
        .send(put_with_cookie(
            &format!("/admin/stores/{first}/package/choice"),
            &json!({ "tenant_id": tenant().as_ulid().to_string(), "package_version_id": V2 }),
            &app.cookie,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.running(first).await.as_deref(), Some(V2));
    assert_eq!(app.running(second).await.as_deref(), Some(V1));
}
