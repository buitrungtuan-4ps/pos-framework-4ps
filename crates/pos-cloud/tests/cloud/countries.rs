// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Countries and locales (`/admin/countries`, `/admin/locales`): read-only master data (ADR-0074).

use axum::http::StatusCode;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie, app_all, clock,
    get, get_with_cookie, json_body, provisioned_admin,
};

fn country_app(admin: FakeAdmin) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::country_router(
        &pos_cloud::countries::registry(),
        admin,
        clock(),
    ))
}

#[tokio::test]
async fn countries_and_locales_are_read_only_master_data() {
    let router = country_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;

    // The default build compiles in the reference module, so the catalogue is non-empty.
    let countries = router
        .clone()
        .oneshot(get_with_cookie("/admin/countries", &cookie))
        .await
        .expect("route countries");
    assert_eq!(countries.status(), StatusCode::OK);
    let body = json_body(countries).await;
    let list = body.as_array().expect("array");
    assert!(
        !list.is_empty(),
        "the reference country module is compiled in"
    );
    assert!(
        list.iter()
            .all(|country| country["currency_code"].is_string()),
        "each country carries a currency"
    );

    // `en` is always in the locale catalogue (the enforced fallback).
    let locales = router
        .clone()
        .oneshot(get_with_cookie("/admin/locales", &cookie))
        .await
        .expect("route locales");
    assert_eq!(locales.status(), StatusCode::OK);
    let langs = json_body(locales).await;
    assert!(
        langs
            .as_array()
            .expect("array")
            .iter()
            .any(|lang| lang == "en"),
        "en is the enforced fallback locale"
    );

    // Read-only master data still requires a session.
    let anon = router
        .oneshot(get("/admin/countries", None))
        .await
        .expect("route anon");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_country_lists_the_settings_its_pack_gives_a_new_store() {
    // ADR-0183 decision 4: Store settings offers a pack's setting defaults when the country is
    // chosen, so the catalogue has to carry them — Vietnam's 20,000 ₫ over/short threshold.
    let router = country_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;
    let body = json_body(
        router
            .oneshot(get_with_cookie("/admin/countries", &cookie))
            .await
            .expect("route countries"),
    )
    .await;
    let list = body.as_array().expect("array");
    let vietnam = list
        .iter()
        .find(|country| country["code"] == "VN")
        .expect("the Vietnam pack is compiled in");
    assert_eq!(
        vietnam["setting_defaults"],
        serde_json::json!([{ "setting_key": "shift.variance_reason_minor", "value": 20_000 }]),
    );
    assert!(
        list.iter()
            .all(|country| country["setting_defaults"].is_array()),
        "a pack that gives none lists an empty array"
    );
}
