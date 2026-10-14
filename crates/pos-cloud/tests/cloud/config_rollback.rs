// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Reads after a rollback: what the store runs, without its settings.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::config_tree::{
    CapabilityValidator, ConfigLevel, ConfigTree, ConfigTreeState, ConfigTreeStore,
};
use pos_cloud::fees::InMemoryFeeRules;
use pos_cloud::registry::EntityStatus;
use pos_cloud::store_groups::{StoreGroup, StoreGroupId};
use pos_cloud::version::{Version, Versioned};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeCampaigns, FakeCatalog, FakeConfigTrees, FakeFloor, FakeInventory, FakeKeys,
    FakePeople, FakeReasonCodes, FakeRegistry, FakeReleases, FakeRollups, FakeScheduledPublishes,
    FakeStoreGroups, FakeTaxRates, FakeWebhooks, admin_cookie, app_all, author_a_menu, clock,
    config_release_app, get_with_cookie, json_body, post_with_cookie, provisioned_admin,
    put_with_cookie, seed_store, sent_to, settings_app, settings_stores, store_group_app, tenant,
};

// --- Reads after a rollback: what the store runs, without its settings ------------------------

/// The store's version before the rollback: its `tax`, `locale` and QR guardrails, as the console
/// publishes them on the Store layer.
fn restored_store_layer() -> serde_json::Value {
    serde_json::json!({
        "tax": { "rates": [] },
        "locale": {
            "currency_code": "VND",
            "timezone": "Asia/Ho_Chi_Minh",
            "cutoff_hour": 4,
        },
        "qr": {
            "staff_confirmation_required": false,
            "per_table_limit": 5,
            "rate_window_secs": 60,
        },
    })
}

/// The settings compile's fields on the `qr` and `locale` nodes, which it writes on the Tenant layer.
fn tenant_layer_settings() -> serde_json::Value {
    serde_json::json!({
        "qr": { "table_order": "TABLE_ORDER_JOIN" },
        "locale": { "tax_rounding": "TAX_ROUNDING_DOWN" },
    })
}

/// Gives a store a tree as a rollback leaves it: the settings on the Tenant layer, then
/// [`restored_store_layer`], then a publish that changed the guardrails, rolled back
/// ([`ConfigTree::restore`], the rollback route's own restore). The restored document is on the
/// Tenant layer and the Store layer is empty.
fn seed_rolled_back(config_trees: &FakeConfigTrees, tenant_id: TenantId, store_id: StoreId) {
    let version = |n: u128| ConfigVersionId::new(Ulid::from_u128(n));
    let mut tree = ConfigTree::new(store_id, CapabilityValidator);
    tree.publish(
        ConfigLevel::Tenant,
        tenant_layer_settings(),
        version(1),
        Vec::new(),
    )
    .expect("the settings publish");
    tree.publish(
        ConfigLevel::Store,
        restored_store_layer(),
        version(2),
        Vec::new(),
    )
    .expect("the store's publish");
    let mut changed = restored_store_layer();
    changed["qr"]["per_table_limit"] = serde_json::json!(9);
    tree.publish(ConfigLevel::Store, changed, version(3), Vec::new())
        .expect("the publish rolled back");
    tree.restore(version(2), version(4)).expect("the rollback");
    let state = tree.state();
    assert_eq!(
        state.layers[2],
        serde_json::json!({}),
        "a rollback empties the Store layer"
    );
    let version = config_trees.mint();
    config_trees
        .rows
        .lock()
        .expect("lock")
        .insert((tenant_id, store_id), Versioned::new(state, version));
}

/// The console's node forms over `config_trees`: the QR guardrails among them.
fn config_form_app(admin: FakeAdmin, config_trees: FakeConfigTrees) -> axum::Router {
    http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    ))
    .merge(http::config_channels_router(
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// The release routes, with the catalog routes over the same catalog, so a release can carry a menu
/// authored in the test.
fn menu_release_app(
    registry: FakeRegistry,
    config_trees: FakeConfigTrees,
    scheduled: FakeScheduledPublishes,
) -> axum::Router {
    let admin = provisioned_admin();
    let catalog = FakeCatalog::default();
    http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    ))
    .merge(http::catalog_router(
        catalog.clone(),
        admin.clone(),
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
    .merge(http::config_release_router(
        FakeReleases::default(),
        scheduled,
        FakeStoreGroups::default(),
        registry.clone(),
        config_trees,
        http::batch_nodes(
            catalog,
            FakeTaxRates::default(),
            FakeCampaigns,
            FakeInventory::default(),
            FakeReasonCodes::default(),
            FakePeople::default(),
            FakeFloor::default(),
            InMemoryFeeRules::new(),
            registry,
        ),
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// Creates a release for `store` and schedules it carrying `nodes`, and returns the schedule's
/// response. `wall_clock` gives the release a Monday-04:00 local moment; otherwise it is instant.
async fn schedule_a_release(
    router: &axum::Router,
    cookie: &str,
    tenant_id: TenantId,
    store: StoreId,
    wall_clock: bool,
    nodes: serde_json::Value,
) -> axum::response::Response {
    let tenant = tenant_id.as_ulid().to_string();
    let moment = if wall_clock {
        serde_json::json!({ "wall_clock_date": "2027-02-08", "wall_clock_time": "04:00" })
    } else {
        serde_json::json!({ "instant_at_ms": 1_800_000_000_000_i64 })
    };
    let mut draft = serde_json::json!({ "tenant_id": tenant, "name": "After the rollback" });
    if let (Some(draft), Some(moment)) = (draft.as_object_mut(), moment.as_object()) {
        draft.extend(moment.clone());
    }
    let created = router
        .clone()
        .oneshot(post_with_cookie("/admin/config-releases", &draft, cookie))
        .await
        .expect("route the create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let release_id = json_body(created).await["release_id"]
        .as_str()
        .expect("release_id")
        .to_owned();
    router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/config-releases/{release_id}/schedule"),
            &serde_json::json!({
                "tenant_id": tenant,
                "nodes": nodes,
                "store_ids": [store.as_ulid().to_string()],
            }),
            cookie,
        ))
        .await
        .expect("route the schedule")
}

/// After a rollback the QR form reads the guardrails the store runs, which the rollback restored,
/// rather than nothing; and not the setting beside them.
#[tokio::test]
async fn after_a_rollback_the_qr_form_reads_the_restored_guardrails() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let store = StoreId::new(Ulid::from_u128(0x2B01));
    let config_trees = FakeConfigTrees::default();
    seed_rolled_back(&config_trees, tenant_id, store);
    let router = config_form_app(provisioned_admin(), config_trees);
    let cookie = admin_cookie(&router).await;

    let read = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/config/qr?tenant_id={}&store_id={}",
                tenant_id.as_ulid(),
                store.as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(
        json_body(read).await,
        restored_store_layer()["qr"],
        "the restored guardrails, without the `table_order` setting the store runs beside them"
    );
}

/// After a rollback a wall-clock release finds the timezone the store runs, and times the store by
/// it, rather than refusing it as a store with no published locale.
#[tokio::test]
async fn after_a_rollback_a_wall_clock_release_reads_the_restored_timezone() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let store = StoreId::new(Ulid::from_u128(0x2B02));
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store, true);
    let config_trees = FakeConfigTrees::default();
    seed_rolled_back(&config_trees, tenant_id, store);
    let router = config_release_app(
        provisioned_admin(),
        FakeReleases::default(),
        FakeScheduledPublishes::default(),
        FakeStoreGroups::default(),
        registry,
        config_trees,
    );
    let cookie = admin_cookie(&router).await;

    let scheduled = schedule_a_release(
        &router,
        &cookie,
        tenant_id,
        store,
        true,
        serde_json::json!([{ "node": "campaigns", "arguments": {} }]),
    )
    .await;
    assert_eq!(scheduled.status(), StatusCode::OK);
    let report = json_body(scheduled).await;
    // Monday 04:00 in Ho Chi Minh City (UTC+7) is 21:00 UTC the day before.
    assert_eq!(
        report["pairs"][0]["effective_at_ms"].as_i64(),
        Some(1_802_034_000_000),
        "{report}"
    );
}

/// After a rollback a `menu` batch finds the `tax` and `locale` the store runs, and publishes to it
/// rather than skipping it.
#[tokio::test]
async fn after_a_rollback_a_menu_batch_finds_the_restored_tax_and_locale() {
    let tenant_id = TenantId::new(Ulid::from_u128(1));
    let store = StoreId::new(Ulid::from_u128(0x2B03));
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store, true);
    let config_trees = FakeConfigTrees::default();
    seed_rolled_back(&config_trees, tenant_id, store);
    let group_id = StoreGroupId::new(Ulid::from_u128(0x2B10));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Rolled back".to_owned(),
            status: EntityStatus::Active,
        },
        &[store],
    );
    let router = store_group_app(
        provisioned_admin(),
        groups,
        registry,
        config_trees,
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let menu_id = author_a_menu(&router, &cookie, &tenant).await;

    let published = router
        .oneshot(post_with_cookie(
            &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
            &serde_json::json!({
                "tenant_id": tenant,
                "node": "menu",
                "arguments": { "menu_id": menu_id },
            }),
            &cookie,
        ))
        .await
        .expect("route the batch publish");
    assert_eq!(published.status(), StatusCode::OK);
    let report = json_body(published).await;
    assert_eq!(report["results"][0]["outcome"], "applied", "{report}");
}

/// After a rollback a release carrying a `menu` finds the `tax` and `locale` the store runs, and is
/// scheduled rather than refused for want of them.
#[tokio::test]
async fn after_a_rollback_a_release_carrying_a_menu_finds_the_restored_tax_and_locale() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let store = StoreId::new(Ulid::from_u128(0x2B04));
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store, true);
    let config_trees = FakeConfigTrees::default();
    seed_rolled_back(&config_trees, tenant_id, store);
    let scheduled = FakeScheduledPublishes::default();
    let router = menu_release_app(registry, config_trees, scheduled.clone());
    let cookie = admin_cookie(&router).await;
    let menu_id = author_a_menu(&router, &cookie, &tenant_id.as_ulid().to_string()).await;

    let response = schedule_a_release(
        &router,
        &cookie,
        tenant_id,
        store,
        false,
        serde_json::json!([{ "node": "menu", "arguments": { "menu_id": menu_id } }]),
    )
    .await;
    let status = response.status();
    assert_eq!(status, StatusCode::OK, "{}", json_body(response).await);
    let rows = scheduled.rows.lock().expect("lock");
    assert!(
        rows.iter()
            .any(|row| row.store_id == store && row.node_key == "menu"),
        "the menu is scheduled for the store"
    );
}

/// A node that holds nothing but settings still reads as no node: the QR form reads `null`, a
/// `menu` batch skips the store for want of `locale`, and a release carrying a menu is refused for
/// it, as before.
#[tokio::test]
async fn a_node_holding_only_settings_still_reads_as_no_node() {
    let tenant_id = TenantId::new(Ulid::from_u128(1));
    let store = StoreId::new(Ulid::from_u128(0x2B05));
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store, true);
    let config_trees = FakeConfigTrees::default();
    // The settings compile has written `qr.table_order` and `locale.tax_rounding`; the console has
    // published a tax table and nothing else.
    config_trees.rows.lock().expect("lock").insert(
        (tenant_id, store),
        Versioned::new(
            ConfigTreeState {
                layers: [
                    tenant_layer_settings(),
                    serde_json::json!({}),
                    serde_json::json!({ "tax": { "rates": [] } }),
                    serde_json::json!({}),
                ],
                history: Vec::new(),
                k: 8,
            },
            Version::new("seed"),
        ),
    );

    let forms = config_form_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&forms).await;
    let read = forms
        .oneshot(get_with_cookie(
            &format!(
                "/admin/config/qr?tenant_id={}&store_id={}",
                tenant_id.as_ulid(),
                store.as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(json_body(read).await, serde_json::Value::Null);

    let group_id = StoreGroupId::new(Ulid::from_u128(0x2B11));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Settings only".to_owned(),
            status: EntityStatus::Active,
        },
        &[store],
    );
    let batch = store_group_app(
        provisioned_admin(),
        groups,
        registry.clone(),
        config_trees.clone(),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&batch).await;
    let tenant = tenant_id.as_ulid().to_string();
    let menu_id = author_a_menu(&batch, &cookie, &tenant).await;
    let report = json_body(
        batch
            .oneshot(post_with_cookie(
                &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
                &serde_json::json!({
                    "tenant_id": tenant,
                    "node": "menu",
                    "arguments": { "menu_id": menu_id },
                }),
                &cookie,
            ))
            .await
            .expect("route the batch publish"),
    )
    .await;
    assert_eq!(report["results"][0]["outcome"], "skipped", "{report}");
    assert!(
        report["results"][0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("`locale`")),
        "{report}"
    );

    let releases = menu_release_app(registry, config_trees, FakeScheduledPublishes::default());
    let cookie = admin_cookie(&releases).await;
    let menu_id = author_a_menu(&releases, &cookie, &tenant).await;
    let refused = schedule_a_release(
        &releases,
        &cookie,
        tenant_id,
        store,
        false,
        serde_json::json!([{ "node": "menu", "arguments": { "menu_id": menu_id } }]),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let message = json_body(refused).await["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(message.contains("`locale`"), "{message}");
}

/// A form never reads a settings field: the QR form leaves out the `table_order` a settings write
/// put on the node, which the store runs beside the guardrails.
#[tokio::test]
async fn a_form_read_leaves_out_the_settings_on_its_node() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();
    let guardrails = restored_store_layer()["qr"].clone();
    config_trees.seed_store_layer(tenant(), first, serde_json::json!({ "qr": guardrails }));
    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "setting_key": "qr.table_order",
                "scope": "SETTING_SCOPE_STORE",
                "scope_id": first.to_string(),
                "value": "TABLE_ORDER_JOIN",
            }),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    let tree = config_trees
        .load(tenant(), first)
        .await
        .expect("load")
        .expect("a tree");
    assert_eq!(
        sent_to(&tree.record)["qr"]["table_order"],
        "TABLE_ORDER_JOIN",
        "the store runs the setting"
    );

    let read = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/config/qr?tenant_id={}&store_id={}",
                tenant().as_ulid(),
                first.as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(
        json_body(read).await,
        guardrails,
        "and the form does not show it"
    );
}
