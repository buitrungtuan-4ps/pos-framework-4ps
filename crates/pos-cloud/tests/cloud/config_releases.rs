// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Config releases (`/admin/config-releases`, ADR-0125): a wall-clock or instant release, its
//! timezones, its prerequisites and its report.

use axum::http::StatusCode;
use pos_cloud::config_tree::ConfigTreeState;
use pos_cloud::registry::EntityStatus;
use pos_cloud::store_groups::{StoreGroup, StoreGroupId};
use pos_cloud::version::{Version, Versioned};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, FakeRegistry, FakeReleases, FakeScheduledPublishes, FakeStoreGroups,
    admin_cookie, config_release_app, draft_a_release, get_with_cookie, json_body,
    post_with_cookie, provisioned_admin, seed_store,
};

/// Gives a store a published `locale` carrying `timezone`, which is what a wall-clock release reads.
fn seed_timezone(
    config_trees: &FakeConfigTrees,
    tenant_id: TenantId,
    store_id: StoreId,
    timezone: Option<&str>,
) {
    let locale = timezone.map_or_else(
        || serde_json::json!({ "currency_code": "VND" }),
        |zone| serde_json::json!({ "currency_code": "VND", "timezone": zone }),
    );
    config_trees.rows.lock().expect("lock").insert(
        (tenant_id, store_id),
        Versioned::new(
            ConfigTreeState {
                layers: [
                    serde_json::json!({}),
                    serde_json::json!({}),
                    serde_json::json!({ "locale": locale }),
                    serde_json::json!({}),
                ],
                history: Vec::new(),
                k: 8,
            },
            Version::new("seed"),
        ),
    );
}

#[tokio::test]
async fn one_wall_clock_release_becomes_one_instant_per_store_timezone() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let saigon = StoreId::new(Ulid::from_u128(31));
    let tokyo = StoreId::new(Ulid::from_u128(32));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, saigon, true);
    seed_store(&registry, tenant_id, tokyo, true);

    let config_trees = FakeConfigTrees::default();
    seed_timezone(&config_trees, tenant_id, saigon, Some("Asia/Ho_Chi_Minh"));
    seed_timezone(&config_trees, tenant_id, tokyo, Some("Asia/Tokyo"));

    let group_id = StoreGroupId::new(Ulid::from_u128(41));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Tết cohort".to_owned(),
            status: EntityStatus::Active,
        },
        &[saigon, tokyo],
    );

    let scheduled = FakeScheduledPublishes::default();
    let router = config_release_app(
        provisioned_admin(),
        FakeReleases::default(),
        scheduled.clone(),
        groups,
        registry,
        config_trees,
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let group = group_id.as_ulid().to_string();
    let release_id = draft_a_release(&router, &cookie, &tenant, &group).await;

    let scheduled_response = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/config-releases/{release_id}/schedule"),
            &serde_json::json!({
                "tenant_id": tenant,
                "nodes": [{ "node": "campaigns", "arguments": {} }],
            }),
            &cookie,
        ))
        .await
        .expect("route the schedule");
    assert_eq!(scheduled_response.status(), StatusCode::OK);
    let report = json_body(scheduled_response).await;
    assert_eq!(report["release"]["status"], "scheduled");

    let pairs = report["pairs"].as_array().expect("pairs");
    assert_eq!(pairs.len(), 2, "one pair per (node, store)");
    let instant_at = |store: StoreId| -> i64 {
        pairs
            .iter()
            .find(|pair| pair["store_id"] == store.to_string())
            .and_then(|pair| pair["effective_at_ms"].as_i64())
            .expect("an instant for the store")
    };
    // "Monday 04:00, local" is not a time — it is one instant per timezone, and Tokyo (UTC+9)
    // reaches its 04:00 exactly two hours before Ho Chi Minh City (UTC+7). This is finding F17.
    assert_eq!(
        instant_at(saigon) - instant_at(tokyo),
        2 * 60 * 60 * 1_000,
        "the fleet's switchover is two hours wide, and the release says so"
    );
}

#[tokio::test]
async fn a_store_with_no_published_timezone_refuses_a_wall_clock_release_by_name() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let timed = StoreId::new(Ulid::from_u128(33));
    let untimed = StoreId::new(Ulid::from_u128(34));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, timed, true);
    seed_store(&registry, tenant_id, untimed, true);

    let config_trees = FakeConfigTrees::default();
    seed_timezone(&config_trees, tenant_id, timed, Some("Asia/Ho_Chi_Minh"));
    seed_timezone(&config_trees, tenant_id, untimed, None);

    let group_id = StoreGroupId::new(Ulid::from_u128(42));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Half-provisioned".to_owned(),
            status: EntityStatus::Active,
        },
        &[timed, untimed],
    );

    let scheduled = FakeScheduledPublishes::default();
    let router = config_release_app(
        provisioned_admin(),
        FakeReleases::default(),
        scheduled.clone(),
        groups,
        registry,
        config_trees,
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let group = group_id.as_ulid().to_string();
    let release_id = draft_a_release(&router, &cookie, &tenant, &group).await;

    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/config-releases/{release_id}/schedule"),
            &serde_json::json!({
                "tenant_id": tenant,
                "nodes": [{ "node": "campaigns", "arguments": {} }],
            }),
            &cookie,
        ))
        .await
        .expect("route the schedule");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let message = json_body(refused).await["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains(&untimed.to_string()),
        "the refusal names the store that cannot be timed, not just that one exists: {message}"
    );
    // The half-provisioned shop is the whole point of refusing rather than defaulting: quietly
    // assuming UTC would put a Vietnamese publish in the middle of dinner service.
    assert!(
        scheduled.rows.lock().expect("lock").is_empty(),
        "a refused release writes no pairs at all, not even for the store that resolved"
    );
}

#[tokio::test]
async fn an_instant_release_needs_no_timezone_and_cancels_back_to_a_draft() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let being_set_up = StoreId::new(Ulid::from_u128(35));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, being_set_up, true);
    let config_trees = FakeConfigTrees::default();
    seed_timezone(&config_trees, tenant_id, being_set_up, None);

    let scheduled = FakeScheduledPublishes::default();
    let router = config_release_app(
        provisioned_admin(),
        FakeReleases::default(),
        scheduled.clone(),
        FakeStoreGroups::default(),
        registry,
        config_trees,
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config-releases",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Opening day",
                "instant_at_ms": 1_800_000_000_000_i64,
            }),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let release_id = json_body(created).await["release_id"]
        .as_str()
        .expect("release_id")
        .to_owned();

    // No group on the draft, so the store list comes with the schedule — which is also when it is
    // snapshotted (ADR-0125 §3).
    let ok = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/config-releases/{release_id}/schedule"),
            &serde_json::json!({
                "tenant_id": tenant,
                "nodes": [{ "node": "campaigns", "arguments": {} }],
                "store_ids": [being_set_up.as_ulid().to_string()],
            }),
            &cookie,
        ))
        .await
        .expect("route the schedule");
    assert_eq!(
        ok.status(),
        StatusCode::OK,
        "an instant release is the escape hatch for a store whose locale nobody has published"
    );
    let report = json_body(ok).await;
    assert_eq!(report["pairs"].as_array().expect("pairs").len(), 1);
    assert_eq!(report["pairs"][0]["effective_at_ms"], 1_800_000_000_000_i64);

    let cancelled = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/config-releases/{release_id}/cancel"),
            &serde_json::json!({ "tenant_id": tenant }),
            &cookie,
        ))
        .await
        .expect("route the cancel");
    assert_eq!(cancelled.status(), StatusCode::OK);
    let report = json_body(cancelled).await;
    assert_eq!(report["pairs"][0]["status"], "cancelled");
}

#[tokio::test]
async fn a_release_carrying_a_menu_without_its_tax_is_refused_before_anything_is_written() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let store_id = StoreId::new(Ulid::from_u128(36));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);
    let config_trees = FakeConfigTrees::default();
    // A published locale (so the moment resolves) but no `tax`, which `menu` requires (ADR-0122 §7).
    seed_timezone(&config_trees, tenant_id, store_id, Some("Asia/Tokyo"));

    let scheduled = FakeScheduledPublishes::default();
    let router = config_release_app(
        provisioned_admin(),
        FakeReleases::default(),
        scheduled.clone(),
        FakeStoreGroups::default(),
        registry,
        config_trees,
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config-releases",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Menu only",
                "wall_clock_date": "2027-02-08",
                "wall_clock_time": "04:00",
            }),
            &cookie,
        ))
        .await
        .expect("route the create");
    let release_id = json_body(created).await["release_id"]
        .as_str()
        .expect("release_id")
        .to_owned();

    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/config-releases/{release_id}/schedule"),
            &serde_json::json!({
                "tenant_id": tenant,
                "nodes": [{ "node": "menu", "arguments": { "menu_id": Ulid::from_u128(51).to_string() } }],
                "store_ids": [store_id.as_ulid().to_string()],
            }),
            &cookie,
        ))
        .await
        .expect("route the schedule");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let message = json_body(refused).await["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("tax"),
        "the refusal names the node the store is missing: {message}"
    );
    // Told now, while the operator can fix it — the schedule-time half of ADR-0125 §6. The fire-time
    // half is the activator's, because a tax class archived on Sunday is a real sequence.
    assert!(scheduled.rows.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_release_report_shows_only_its_own_pairs() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let store_id = StoreId::new(Ulid::from_u128(37));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);
    let config_trees = FakeConfigTrees::default();
    seed_timezone(&config_trees, tenant_id, store_id, Some("Asia/Tokyo"));

    let scheduled = FakeScheduledPublishes::default();
    let router = config_release_app(
        provisioned_admin(),
        FakeReleases::default(),
        scheduled.clone(),
        FakeStoreGroups::default(),
        registry,
        config_trees,
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();

    let mut ids = Vec::new();
    for name in ["First", "Second"] {
        let created = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/config-releases",
                &serde_json::json!({
                    "tenant_id": tenant,
                    "name": name,
                    "instant_at_ms": 1_800_000_000_000_i64,
                }),
                &cookie,
            ))
            .await
            .expect("route the create");
        let id = json_body(created).await["release_id"]
            .as_str()
            .expect("release_id")
            .to_owned();
        let response = router
            .clone()
            .oneshot(post_with_cookie(
                &format!("/admin/config-releases/{id}/schedule"),
                &serde_json::json!({
                    "tenant_id": tenant,
                    "nodes": [{ "node": "campaigns", "arguments": {} }],
                    "store_ids": [store_id.as_ulid().to_string()],
                }),
                &cookie,
            ))
            .await
            .expect("route the schedule");
        assert_eq!(response.status(), StatusCode::OK);
        ids.push(id);
    }

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config-releases?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the list");
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(
        json_body(listed).await["releases"]
            .as_array()
            .expect("releases")
            .len(),
        2
    );

    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config-releases/{}?tenant_id={tenant}", ids[0]),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(read.status(), StatusCode::OK);
    let report = json_body(read).await;
    let pairs = report["pairs"].as_array().expect("pairs");
    assert_eq!(
        pairs.len(),
        1,
        "the report is one release's, not the tenant's"
    );
    // The snapshot is deliberately not on the wire: a forty-row grid carrying forty menus is both
    // enormous and, for a priced node, T2 material this read has no reason to hand out.
    assert!(pairs[0].get("node_value").is_none());
}
