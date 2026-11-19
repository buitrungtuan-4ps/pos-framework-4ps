// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A setting value a store's country forbids is refused at the write
//! ([ADR-0223](../../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md) decision 3):
//! `weighing.manual_entry` on, for a Vietnamese store or a wider scope that reaches one, while a
//! store whose country allows it, or is not yet known, takes it.

use axum::http::StatusCode;
use pos_proto::ids::StoreId;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, admin_cookie, json_body, put_with_cookie, settings_app, settings_stores,
    tenant,
};

/// Gives `store` a published `locale` naming `country`.
fn in_country(config_trees: &FakeConfigTrees, store: StoreId, country: &str) {
    config_trees.seed_store_layer(
        tenant(),
        store,
        serde_json::json!({ "locale": { "country_code": country, "currency_code": "VND" } }),
    );
}

/// Writes `weighing.manual_entry` = `on` at `scope` for `scope_id`, answering the status and body.
async fn hand_entry(
    router: &axum::Router,
    cookie: &str,
    (scope, scope_id): (&str, String),
    on: bool,
) -> (StatusCode, serde_json::Value) {
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "setting_key": "weighing.manual_entry",
        "scope": scope,
        "scope_id": scope_id,
        "value": on,
    });
    let answered = router
        .clone()
        .oneshot(put_with_cookie("/admin/settings", &body, cookie))
        .await
        .expect("route the write");
    let status = answered.status();
    (status, json_body(answered).await)
}

fn refused_by_country(answer: &(StatusCode, serde_json::Value)) -> bool {
    answer.0 == StatusCode::BAD_REQUEST
        && answer
            .1
            .to_string()
            .contains("SETTING_FORBIDDEN_BY_COUNTRY")
}

#[tokio::test]
async fn a_store_whose_country_forbids_a_typed_weight_cannot_turn_it_on() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, second, _] = settings_stores();
    in_country(&config_trees, first, "VN");
    in_country(&config_trees, second, "ZZ");
    let store = |id: StoreId| ("SETTING_SCOPE_STORE", id.to_string());

    let refused = hand_entry(&router, &cookie, store(first), true).await;
    assert!(refused_by_country(&refused), "{refused:?}");
    // Off is what the law asks, and a country that allows it takes it on.
    assert_eq!(
        hand_entry(&router, &cookie, store(first), false).await.0,
        StatusCode::OK
    );
    assert_eq!(
        hand_entry(&router, &cookie, store(second), true).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_wider_scope_is_refused_when_any_store_it_reaches_is_in_a_forbidding_country() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, second, third] = settings_stores();
    let tenant_wide = ("SETTING_SCOPE_TENANT", tenant().as_ulid().to_string());

    // No store has a country yet: the edge's own pack holds the law, so the write goes through.
    assert_eq!(
        hand_entry(&router, &cookie, tenant_wide.clone(), true)
            .await
            .0,
        StatusCode::OK
    );

    in_country(&config_trees, first, "ZZ");
    in_country(&config_trees, second, "ZZ");
    in_country(&config_trees, third, "VN");
    let refused = hand_entry(&router, &cookie, tenant_wide, true).await;
    assert!(refused_by_country(&refused), "{refused:?}");
    assert!(
        refused.1.to_string().contains(&third.to_string()),
        "{refused:?}"
    );
}
