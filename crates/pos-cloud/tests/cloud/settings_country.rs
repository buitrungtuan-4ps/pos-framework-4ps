// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A new store's values take its country pack's before the register's presets (ADR-0183): a
//! Vietnamese store asks a reason past 20,000 đồng over or short, a store elsewhere or with no
//! country is given what it was before, and a store that set the value keeps it.

use axum::http::StatusCode;
use pos_proto::ids::StoreId;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, admin_cookie, json_body, post_with_cookie, put_with_cookie, settings_app,
    settings_stores, tenant, tenant_layer_node,
};

const VARIANCE_REASON: &str = "shift.variance_reason_minor";

/// Gives `store` a published `locale` naming `country`, as the console's locale publish does.
fn in_country(config_trees: &FakeConfigTrees, store: StoreId, country: &str) {
    config_trees.seed_store_layer(
        tenant(),
        store,
        serde_json::json!({ "locale": { "country_code": country, "currency_code": "VND" } }),
    );
}

/// Applies the new-store values to `store`, answering the settings written.
async fn apply_presets(router: &axum::Router, cookie: &str, store: StoreId) -> serde_json::Value {
    let presets = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store.to_string(),
    });
    let applied = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/settings/presets",
            &presets,
            cookie,
        ))
        .await
        .expect("route the presets");
    assert_eq!(applied.status(), StatusCode::OK);
    json_body(applied).await["applied"].clone()
}

/// What the store's Tenant layer carries for the variance threshold, if anything.
async fn variance_reason(
    config_trees: &FakeConfigTrees,
    store: StoreId,
) -> Option<serde_json::Value> {
    tenant_layer_node(config_trees, store, "shift")
        .await
        .and_then(|shift| shift.get("variance_reason_minor").cloned())
}

fn names(applied: &serde_json::Value, key: &str) -> bool {
    applied
        .as_array()
        .is_some_and(|keys| keys.iter().any(|written| written == key))
}

#[tokio::test]
async fn a_vietnamese_store_starts_asking_a_reason_past_twenty_thousand_dong() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [_, _, third] = settings_stores();
    in_country(&config_trees, third, "VN");

    let applied = apply_presets(&router, &cookie, third).await;
    assert!(names(&applied, VARIANCE_REASON), "{applied}");
    assert!(
        names(&applied, "session.idle_lock_seconds"),
        "the register's presets still apply beside the pack's: {applied}"
    );
    assert_eq!(
        variance_reason(&config_trees, third).await,
        Some(serde_json::json!(20_000)),
        "the Vietnam pack's figure (the owner, 2026-10-29)"
    );
}

#[tokio::test]
async fn a_store_in_a_country_whose_pack_says_nothing_keeps_the_check_off() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [_, _, third] = settings_stores();
    in_country(&config_trees, third, "JP");

    let applied = apply_presets(&router, &cookie, third).await;
    assert!(!names(&applied, VARIANCE_REASON), "{applied}");
    assert_eq!(variance_reason(&config_trees, third).await, None);
}

#[tokio::test]
async fn a_store_with_no_country_is_given_the_presets_alone() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [_, _, third] = settings_stores();

    let applied = apply_presets(&router, &cookie, third).await;
    assert!(!names(&applied, VARIANCE_REASON), "{applied}");
    assert!(names(&applied, "session.idle_lock_seconds"), "{applied}");
    assert_eq!(variance_reason(&config_trees, third).await, None);
}

#[tokio::test]
async fn a_vietnamese_store_that_set_its_own_threshold_keeps_it() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [_, _, third] = settings_stores();
    in_country(&config_trees, third, "VN");
    let own = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "setting_key": VARIANCE_REASON,
        "scope": "SETTING_SCOPE_STORE",
        "scope_id": third.to_string(),
        "value": 50_000,
    });
    let written = router
        .clone()
        .oneshot(put_with_cookie("/admin/settings", &own, &cookie))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);

    let applied = apply_presets(&router, &cookie, third).await;
    assert!(!names(&applied, VARIANCE_REASON), "{applied}");
    assert_eq!(
        variance_reason(&config_trees, third).await,
        Some(serde_json::json!(50_000)),
        "the store's own value, not the pack's"
    );
}
