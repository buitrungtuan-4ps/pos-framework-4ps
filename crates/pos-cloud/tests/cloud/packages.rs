// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The package routes (ADR-0189 decision 1): a package is created, its versions are published
//! once and never change, each carries the minimum edge release the server computed, and only the
//! roles that author may write.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, AuditSink, NoopAuditRecorder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::{Cloud, http};
use pos_cloud_config::packages::{DATA_PART_SINCE, InMemoryPackages};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie,
    app_all, clock, get_with_cookie, json_body, last_trail_entry, post_with_cookie,
    provisioned_admin, role_session_cookie, ulid_text,
};

/// The admin surface plus the package router, on fakes, recording into `audit`.
fn package_app(admin: FakeAdmin, audit: Arc<dyn AuditRecorder>) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::package_router(
        InMemoryPackages::new(),
        admin,
        clock(),
        audit,
    ))
}

const LUNCH: &str = r#"{"time_slot_id":"lunch","label_key":"package.slot.lunch","schedules":[{"days":31,"start_minute":660,"end_minute":840}]}"#;

fn version_body(tenant: &str, label: &str, slots: &str) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant,
        "version_label": label,
        "time_slots": serde_json::from_str::<serde_json::Value>(slots).expect("slots"),
    })
}

async fn send(
    router: &axum::Router,
    request: axum::http::Request<axum::body::Body>,
) -> (StatusCode, serde_json::Value) {
    let response = router.clone().oneshot(request).await.expect("route");
    let status = response.status();
    (status, json_body(response).await)
}

/// A package is created with a server-minted id; each publish stores a new version with its own
/// id and the server's minimum edge release, and the list reads them back oldest first.
#[tokio::test]
async fn a_package_is_created_and_each_publish_is_a_new_version() {
    let audit = FakeAudit::default();
    let router = package_app(provisioned_admin(), Arc::new(AuditSink::new(audit.clone())));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let (status, package) = send(
        &router,
        post_with_cookie(
            "/admin/packages",
            &serde_json::json!({ "tenant_id": tenant, "name": " Counter screens " }),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{package}");
    assert_eq!(package["name"], "Counter screens");
    let package_id = package["package_id"].as_str().expect("an id").to_owned();
    assert_eq!(package_id.len(), 26);

    let versions = format!("/admin/packages/{package_id}/versions");
    let (status, march) = send(
        &router,
        post_with_cookie(
            &versions,
            &version_body(&tenant, "March", &format!("[{LUNCH}]")),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{march}");
    assert_eq!(march["content"]["package_id"], package_id.as_str());
    assert_eq!(march["content"]["version_label"], "March");
    assert_eq!(
        march["content"]["min_edge_version"], "0.14.1",
        "the server computes the minimum edge release; nobody types it"
    );
    let (status, april) = send(
        &router,
        post_with_cookie(&versions, &version_body(&tenant, "April", "[]"), &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{april}");
    assert_ne!(
        march["content"]["package_version_id"],
        april["content"]["package_version_id"]
    );

    let (status, listed) = send(
        &router,
        get_with_cookie(&format!("{versions}?tenant_id={tenant}"), &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut labels: Vec<&str> = listed
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|version| version["content"]["version_label"].as_str())
        .collect();
    // In id order; the test clock mints both in one millisecond, so only the set is certain here.
    labels.sort_unstable();
    assert_eq!(labels, ["April", "March"]);

    let (status, packages) = send(
        &router,
        get_with_cookie(&format!("/admin/packages?tenant_id={tenant}"), &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(packages.as_array().map(Vec::len), Some(1));
    let (_, other) = send(
        &router,
        get_with_cookie(
            &format!("/admin/packages?tenant_id={}", ulid_text(2)),
            &cookie,
        ),
    )
    .await;
    assert_eq!(other, serde_json::json!([]), "another tenant sees none");

    let entry = last_trail_entry(&audit, "packages.version.publish")
        .await
        .expect("the publish is in the trail");
    assert_eq!(entry.entity_type, "package_version");
}

/// Content the edge could not run is refused with the field named, and a version of a package the
/// tenant does not have is not found.
#[tokio::test]
async fn content_that_cannot_run_is_refused_and_an_unknown_package_is_not_found() {
    let router = package_app(provisioned_admin(), Arc::new(NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let (_, package) = send(
        &router,
        post_with_cookie(
            "/admin/packages",
            &serde_json::json!({ "tenant_id": tenant, "name": "Bar" }),
            &cookie,
        ),
    )
    .await;
    let versions = format!(
        "/admin/packages/{}/versions",
        package["package_id"].as_str().expect("an id")
    );

    for (label, slots, field) in [
        (
            "March",
            format!("[{LUNCH},{LUNCH}]"),
            "time_slots.time_slot_id",
        ),
        ("  ", "[]".to_owned(), "version_label"),
        (
            "March",
            format!("[{}]", LUNCH.replace("660", "1500")),
            "time_slots.schedules",
        ),
    ] {
        let (status, body) = send(
            &router,
            post_with_cookie(&versions, &version_body(&tenant, label, &slots), &cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains(field), "{field} named in {body}");
    }

    let (status, _) = send(
        &router,
        post_with_cookie(
            &format!("/admin/packages/{}/versions", ulid_text(99)),
            &version_body(&tenant, "March", "[]"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A viewer reads packages and cannot create one or publish a version.
#[tokio::test]
async fn a_viewer_reads_packages_and_cannot_author_them() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let router = package_app(admin, Arc::new(NoopAuditRecorder));
    let tenant = ulid_text(1);

    let (status, _) = send(
        &router,
        get_with_cookie(&format!("/admin/packages?tenant_id={tenant}"), &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &router,
        post_with_cookie(
            "/admin/packages",
            &serde_json::json!({ "tenant_id": tenant, "name": "Bar" }),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &router,
        post_with_cookie(
            &format!("/admin/packages/{}/versions", ulid_text(9)),
            &version_body(&tenant, "March", "[]"),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A field on an order, choosing from a list. Fake labels only.
const OCCASION: &str = r#"{"custom_fields":[{"custom_field_id":"01J0000000000000000000CF01","object":"DATA_OBJECT_ORDER","label_key":"data.order.occasion","field_type":"CUSTOM_FIELD_TYPE_CHOICE","personal":false,"custom_list_id":"01J0000000000000000000CA01"}],"lists":[{"custom_list_id":"01J0000000000000000000CA01","label_key":"data.list.occasion","entries":[{"list_entry_id":"01J0000000000000000000EE01","label_key":"data.occasion.birthday"}]}]}"#;

fn data_body(tenant: &str, data: &str) -> serde_json::Value {
    let mut body = version_body(tenant, "March", "[]");
    body["data"] = serde_json::from_str(data).expect("data");
    body
}

/// A version with a data part is published and carries the release that checks it; a personal
/// field is refused as unavailable (ADR-0206 decision 11); a later version that drops what an
/// earlier one declared is refused (ADR-0194 decision 6), and one that only adds is published.
#[tokio::test]
async fn a_data_part_is_published_checked_and_only_ever_added_to() {
    let router = package_app(provisioned_admin(), Arc::new(NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let (_, package) = send(
        &router,
        post_with_cookie(
            "/admin/packages",
            &serde_json::json!({ "tenant_id": tenant, "name": "Bar" }),
            &cookie,
        ),
    )
    .await;
    let versions = format!(
        "/admin/packages/{}/versions",
        package["package_id"].as_str().expect("an id")
    );
    let publish = |data: String| {
        let body = data_body(&tenant, &data);
        let request = post_with_cookie(&versions, &body, &cookie);
        send(&router, request)
    };

    let (status, body) = publish(OCCASION.to_owned()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["content"]["min_edge_version"], DATA_PART_SINCE);
    assert_eq!(
        body["content"]["data"]["custom_fields"][0]["custom_field_id"],
        "01J0000000000000000000CF01"
    );

    let personal = OCCASION.replace(
        r#""field_type":"CUSTOM_FIELD_TYPE_CHOICE","personal":false,"custom_list_id":"01J0000000000000000000CA01""#,
        r#""field_type":"CUSTOM_FIELD_TYPE_TEXT","personal":true,"retention_days":30,"consent_purpose_key":"data.consent.order""#,
    );
    let (status, body) = publish(personal).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("PERSONAL_FIELD_UNAVAILABLE"),
        "{body}"
    );
    assert!(
        body.to_string().contains("data.custom_fields.personal"),
        "{body}"
    );

    let (status, body) = publish(r#"{"lists":[]}"#.to_owned()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("NOT_ADDITIVE"), "{body}");

    let added = OCCASION.replace(
        "]}]}",
        r#"]}],"object_types":[{"object_type_id":"01J00000000000000000000B01","label_key":"data.reservation","fields":[{"custom_field_id":"01J0000000000000000000CF02","label_key":"data.reservation.party","field_type":"CUSTOM_FIELD_TYPE_INT","personal":false}]}]}"#,
    );
    let (status, body) = publish(added).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}
