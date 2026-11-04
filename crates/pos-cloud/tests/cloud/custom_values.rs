// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The custom-data report (ADR-0194 decision 7): a summary groups and filters by non-personal
//! fields only, and refuses a field any package version declares personal.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::{Cloud, http};
use pos_cloud_config::packages::{
    InMemoryPackages, PackageRecord, PackageStore, PackageVersionRecord,
};
use pos_cloud_reports::custom_data::{CustomValueSummary, SummaryQuery, ValueCount};
use pos_fakes::FakeStore;
use pos_ports::PortError;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::text::DisplayName;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie, app_all, clock,
    get_with_cookie, json_body, provisioned_admin, ulid_text,
};

/// The projection's answer, and the queries it was asked, so a test can see what reached it.
#[derive(Clone, Default)]
struct FakeSummary {
    asked: Arc<Mutex<Vec<SummaryQuery>>>,
}

impl CustomValueSummary for FakeSummary {
    async fn summary(
        &self,
        _: TenantId,
        _: StoreId,
        query: &SummaryQuery,
        _: usize,
    ) -> Result<Vec<ValueCount>, PortError> {
        self.asked.lock().expect("lock").push(*query);
        Ok(vec![ValueCount {
            value: serde_json::from_value(serde_json::json!({
                "custom_field_id": query.custom_field_id,
                "list_entry_id": "01J0000000000000000000EE01",
            }))
            .expect("a value"),
            objects: 3,
        }])
    }
}

/// An order's occasion (a choice), a reservation's party size (an int) and contact (personal text).
/// The last could not be published through the route; it is written straight to the store here to
/// prove the report refuses it anyway. Fake labels only.
const DATA: &str = r#"{"custom_fields":[{"custom_field_id":"01J0000000000000000000CF01","object":"DATA_OBJECT_ORDER","label_key":"data.order.occasion","field_type":"CUSTOM_FIELD_TYPE_CHOICE","personal":false,"custom_list_id":"01J0000000000000000000CA01"}],"object_types":[{"object_type_id":"01J00000000000000000000B01","label_key":"data.reservation","fields":[{"custom_field_id":"01J0000000000000000000CF02","label_key":"data.reservation.party","field_type":"CUSTOM_FIELD_TYPE_INT","personal":false},{"custom_field_id":"01J0000000000000000000CF03","label_key":"data.reservation.contact","field_type":"CUSTOM_FIELD_TYPE_TEXT","personal":true,"retention_days":30,"consent_purpose_key":"data.consent.reservation"}]}],"lists":[{"custom_list_id":"01J0000000000000000000CA01","label_key":"data.list.occasion","entries":[{"list_entry_id":"01J0000000000000000000EE01","label_key":"data.occasion.birthday"}]}]}"#;

async fn report_app(summary: FakeSummary) -> (axum::Router, String) {
    let tenant = TenantId::new(ulid_text(1).parse().expect("ulid"));
    let packages = InMemoryPackages::new();
    let package = PackageRecord {
        package_id: "01J00000000000000000000P01".parse().expect("id"),
        name: DisplayName::new("Bar"),
        create_time: clock().now(),
    };
    packages
        .create_package(tenant, &package)
        .await
        .expect("create");
    let version: PackageVersionRecord = serde_json::from_value(serde_json::json!({
        "content": {
            "package_id": "01J00000000000000000000P01",
            "package_version_id": "01J00000000000000000000V01",
            "version_label": "March",
            "min_edge_version": "0.14.1",
            "data": serde_json::from_str::<serde_json::Value>(DATA).expect("data"),
        },
        "create_time": serde_json::to_value(clock().now()).expect("time"),
    }))
    .expect("a version");
    packages
        .publish_version(tenant, &version)
        .await
        .expect("publish");
    let admin: FakeAdmin = provisioned_admin();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    let router = http::router(app).merge(http::custom_value_report_router(
        summary,
        packages,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ));
    let cookie = admin_cookie(&router).await;
    (router, cookie)
}

async fn get(router: &axum::Router, uri: &str, cookie: &str) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(get_with_cookie(uri, cookie))
        .await
        .expect("route");
    let status = response.status();
    (status, json_body(response).await)
}

fn summary_uri(query: &str) -> String {
    format!(
        "/admin/stores/{}/custom-values/summary?tenant_id={}&{query}",
        ulid_text(2),
        ulid_text(1)
    )
}

#[tokio::test]
async fn a_summary_groups_by_a_non_personal_field_and_filters_by_another() {
    let summary = FakeSummary::default();
    let (router, cookie) = report_app(summary.clone()).await;

    let (status, body) = get(
        &router,
        &summary_uri("object=DATA_OBJECT_ORDER&custom_field_id=01J0000000000000000000CF01"),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["groups"][0]["objects"], 3);
    assert_eq!(body["truncated"], false);
    assert!(!body.to_string().contains("subject"), "{body}");

    let (status, body) = get(
        &router,
        &summary_uri(
            "object_type_id=01J00000000000000000000B01&custom_field_id=01J0000000000000000000CF02\
             &filter_field_id=01J0000000000000000000CF02&filter_value=4",
        ),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(summary.asked.lock().expect("lock").len(), 2);
}

#[tokio::test]
async fn a_personal_or_unknown_field_or_a_bad_scope_is_refused() {
    let summary = FakeSummary::default();
    let (router, cookie) = report_app(summary.clone()).await;
    let reservation = "object_type_id=01J00000000000000000000B01";
    for (query, code) in [
        (
            format!("{reservation}&custom_field_id=01J0000000000000000000CF03"),
            "PERSONAL_FIELD",
        ),
        (
            format!(
                "{reservation}&custom_field_id=01J0000000000000000000CF02\
                 &filter_field_id=01J0000000000000000000CF03&filter_value=x"
            ),
            "PERSONAL_FIELD",
        ),
        (
            format!("{reservation}&custom_field_id=01J0000000000000000000CF09"),
            "UNKNOWN",
        ),
        (
            "custom_field_id=01J0000000000000000000CF02".to_owned(),
            "REQUIRED",
        ),
        (
            format!(
                "{reservation}&custom_field_id=01J0000000000000000000CF02\
                 &filter_field_id=01J0000000000000000000CF02&filter_value=four"
            ),
            "INVALID",
        ),
    ] {
        let (status, body) = get(&router, &summary_uri(&query), &cookie).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert!(body.to_string().contains(code), "{query}: {code} in {body}");
    }
    assert!(
        summary.asked.lock().expect("lock").is_empty(),
        "a refused summary never reaches the projection"
    );
}
