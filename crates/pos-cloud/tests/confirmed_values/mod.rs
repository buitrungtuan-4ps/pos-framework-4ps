// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `shift.drawer_access = DRAWER_ACCESS_ASSIGNED`, the register's first value written only with a
//! recorded confirmation
//! ([ADR-0169](../../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)),
//! over the shipped routes:
//!
//! - `PUT /admin/settings` refuses it without `notice_confirmed`, writes nothing, and with it
//!   records the notice beside the value (decision 2);
//! - the layer write and the rollback cannot make a store run it, and a layer that keeps it as the
//!   store already runs it passes (decision 3);
//! - a node publish and a release, which carry no `shift` node, still reach a store that runs it.
//!   That they would be refused too, were one to carry it, is the check's own test with a register
//!   of its own (`scheduling::confirm`, `http::setting_refusal`).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditSink, AuditStore};
use pos_cloud::config_tree::{ConfigTreeState, ConfigTreeStore};
use pos_cloud::registry::EntityStatus;
use pos_cloud::store_groups::{StoreGroup, StoreGroupId};
use pos_cloud::version::{Version, Versioned};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use super::{
    FakeAudit, FakeConfigTrees, FakeReleases, FakeScheduledPublishes, FakeStoreGroups,
    admin_cookie, config_release_app, delete_with_cookie, draft_a_release, json_body,
    post_config_with_etag, post_with_cookie, provisioned_admin, put_with_cookie, put_with_etag,
    seed_store, settings_app_with, settings_stores, tenant,
};

const DRAWER_ACCESS: &str = "shift.drawer_access";
const ASSIGNED: &str = "DRAWER_ACCESS_ASSIGNED";
const NOTICE: &str = "settings.item.shift.drawer_access.confirm";

/// A `PUT /admin/settings` body writing `value` for `shift.drawer_access`, confirmed or not.
fn drawer_access(
    scope: &str,
    scope_id: &str,
    value: &str,
    confirmed: Option<bool>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "setting_key": DRAWER_ACCESS,
        "scope": scope,
        "scope_id": scope_id,
        "value": value,
    });
    if let Some(confirmed) = confirmed {
        body["notice_confirmed"] = serde_json::json!(confirmed);
    }
    body
}

/// What `store_id` runs for `shift.drawer_access`, read from its tree's layers as they merge.
async fn runs(config_trees: &FakeConfigTrees, store_id: StoreId) -> Option<String> {
    let tree = config_trees.load(tenant(), store_id).await.expect("load")?;
    tree.record.effective()["shift"]["drawer_access"]
        .as_str()
        .map(str::to_owned)
}

/// The field and reason of a refusal's one detail.
fn refused_on(body: &serde_json::Value) -> (String, String) {
    let detail = &body["error"]["details"][0];
    (
        detail["field"].as_str().unwrap_or_default().to_owned(),
        detail["reason"].as_str().unwrap_or_default().to_owned(),
    )
}

/// The config version the write `body` answered with for `store_id`.
fn version_at(body: &serde_json::Value, store_id: StoreId) -> String {
    body["stores"]
        .as_array()
        .expect("a list of stores")
        .iter()
        .find(|store| store["store_id"] == store_id.to_string())
        .and_then(|store| store["config_version_id"].as_str())
        .expect("the store was published")
        .to_owned()
}

#[tokio::test]
async fn the_settings_write_refuses_it_unconfirmed_and_records_the_notice_when_confirmed() {
    let audit = FakeAudit::default();
    let (router, config_trees, _) = settings_app_with(Arc::new(AuditSink::new(audit.clone())));
    let cookie = admin_cookie(&router).await;
    let every_store = tenant().as_ulid().to_string();

    for unconfirmed in [None, Some(false)] {
        let refused = router
            .clone()
            .oneshot(put_with_cookie(
                "/admin/settings",
                &drawer_access("SETTING_SCOPE_TENANT", &every_store, ASSIGNED, unconfirmed),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            refused_on(&json_body(refused).await),
            (
                "notice_confirmed".to_owned(),
                "CONFIRMATION_REQUIRED".to_owned()
            )
        );
    }
    for store_id in settings_stores() {
        assert_eq!(
            runs(&config_trees, store_id).await,
            None,
            "nothing was written"
        );
    }
    assert!(audit.list(None, 10).await.expect("list").is_empty());

    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &drawer_access("SETTING_SCOPE_TENANT", &every_store, ASSIGNED, Some(true)),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    for store_id in settings_stores() {
        assert_eq!(
            runs(&config_trees, store_id).await.as_deref(),
            Some(ASSIGNED)
        );
    }
    let entries = audit.list(None, 10).await.expect("list");
    let [entry] = entries.as_slice() else {
        panic!("one entry for the write: {entries:?}");
    };
    assert_eq!(entry.action, "setting.set");
    assert_eq!(
        entry.after,
        Some(serde_json::json!({ "value": ASSIGNED, "notice_confirmed": true, "notice": NOTICE }))
    );

    // Shared needs no confirmation.
    let shared = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &drawer_access(
                "SETTING_SCOPE_TENANT",
                &every_store,
                "DRAWER_ACCESS_SHARED",
                None,
            ),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(shared.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_layer_write_cannot_turn_it_on_and_may_keep_it_once_settings_has() {
    let (router, config_trees, _) =
        settings_app_with(Arc::new(pos_cloud::audit::NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let [store_id, ..] = settings_stores();
    let layer_uri = format!(
        "/admin/stores/{}/config/store?tenant_id={}",
        store_id.as_ulid(),
        tenant().as_ulid()
    );
    let layer = serde_json::json!({ "shift": { "drawer_access": ASSIGNED } });

    let refused = router
        .clone()
        .oneshot(put_with_etag(&layer_uri, &layer, &cookie, "*"))
        .await
        .expect("route the layer write");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(
        refused_on(&body),
        (DRAWER_ACCESS.to_owned(), "CONFIRMATION_REQUIRED".to_owned())
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Settings")),
        "{body}"
    );
    assert_eq!(
        runs(&config_trees, store_id).await,
        None,
        "nothing was saved"
    );

    // Confirmed on Settings for every store, the layer may say the same.
    let confirmed = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &drawer_access(
                "SETTING_SCOPE_TENANT",
                &tenant().as_ulid().to_string(),
                ASSIGNED,
                Some(true),
            ),
            &cookie,
        ))
        .await
        .expect("route the write");
    let version = version_at(&json_body(confirmed).await, store_id);
    let kept = router
        .clone()
        .oneshot(put_with_etag(&layer_uri, &layer, &cookie, &version))
        .await
        .expect("route the layer write");
    assert_eq!(kept.status(), StatusCode::OK, "{}", json_body(kept).await);
}

#[tokio::test]
async fn a_rollback_to_a_version_that_ran_it_waits_for_settings() {
    let (router, config_trees, _) =
        settings_app_with(Arc::new(pos_cloud::audit::NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let [store_id, ..] = settings_stores();
    let store = store_id.as_ulid().to_string();

    let confirmed = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &drawer_access("SETTING_SCOPE_STORE", &store, ASSIGNED, Some(true)),
            &cookie,
        ))
        .await
        .expect("route the write");
    let ran_it = version_at(&json_body(confirmed).await, store_id);
    let cleared = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!(
                "/admin/settings?tenant_id={}&setting_key={DRAWER_ACCESS}\
                 &scope=SETTING_SCOPE_STORE&scope_id={store}",
                tenant().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the clear");
    let current = version_at(&json_body(cleared).await, store_id);
    assert_eq!(runs(&config_trees, store_id).await, None);

    let rollback = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!(
                "/admin/stores/{store}/config/rollback?tenant_id={}",
                tenant().as_ulid()
            ),
            &serde_json::json!({ "version_id": ran_it }),
            &cookie,
            &current,
        ))
        .await
        .expect("route the rollback");
    assert_eq!(rollback.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        refused_on(&json_body(rollback).await),
        (DRAWER_ACCESS.to_owned(), "CONFIRMATION_REQUIRED".to_owned())
    );
    assert_eq!(
        runs(&config_trees, store_id).await,
        None,
        "nothing was restored"
    );
}

#[tokio::test]
async fn a_node_publish_still_reaches_a_store_that_runs_it() {
    let (router, config_trees, _) =
        settings_app_with(Arc::new(pos_cloud::audit::NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let [store_id, ..] = settings_stores();
    router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &drawer_access(
                "SETTING_SCOPE_TENANT",
                &tenant().as_ulid().to_string(),
                ASSIGNED,
                Some(true),
            ),
            &cookie,
        ))
        .await
        .expect("route the write");

    let published = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/tender",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id.to_string(),
                "accepted": ["PAYMENT_METHOD_CASH", "PAYMENT_METHOD_CARD"],
            }),
            &cookie,
        ))
        .await
        .expect("route the tender publish");
    assert_eq!(published.status(), StatusCode::OK);
    assert_eq!(
        runs(&config_trees, store_id).await.as_deref(),
        Some(ASSIGNED)
    );
}

#[tokio::test]
async fn a_release_is_still_scheduled_for_a_store_that_runs_it() {
    let tenant_id = TenantId::new(Ulid::from_u128(7));
    let store_id = StoreId::new(Ulid::from_u128(31));
    let registry = super::FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);
    // A store that runs it, confirmed on Settings, with the timezone a wall-clock release reads.
    let config_trees = FakeConfigTrees::default();
    config_trees.rows.lock().expect("lock").insert(
        (tenant_id, store_id),
        Versioned::new(
            ConfigTreeState {
                layers: [
                    serde_json::json!({ "shift": { "drawer_access": ASSIGNED } }),
                    serde_json::json!({}),
                    serde_json::json!({
                        "locale": { "currency_code": "VND", "timezone": "Asia/Ho_Chi_Minh" },
                    }),
                    serde_json::json!({}),
                ],
                history: Vec::new(),
                k: 8,
            },
            Version::new("seed"),
        ),
    );
    let group_id = StoreGroupId::new(Ulid::from_u128(41));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Saigon".to_owned(),
            status: EntityStatus::Active,
        },
        &[store_id],
    );
    let router = config_release_app(
        provisioned_admin(),
        FakeReleases::default(),
        FakeScheduledPublishes::default(),
        groups,
        registry,
        config_trees,
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let release_id =
        draft_a_release(&router, &cookie, &tenant, &group_id.as_ulid().to_string()).await;

    let scheduled = router
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
    assert_eq!(scheduled.status(), StatusCode::OK);
}
