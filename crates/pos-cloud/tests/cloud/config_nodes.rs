// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The per-store config publishes: tax, the store profile, locale, event retention and origins
//! (ADR-0074, ADR-0106, ADR-0145, ADR-0111).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::config_tree::ConfigTreeStore;
use pos_cloud::tax::TaxRateEntry;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::TaxClassId;
use pos_proto::locale::TaxRate;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeRollups, FakeTaxRates, FakeWebhooks, admin_cookie,
    app_all, clock, get_with_cookie, json_body, provisioned_admin, put_with_cookie, store_id,
    tenant,
};

fn tax_publish_app(
    admin: FakeAdmin,
    tax_rates: FakeTaxRates,
    config_trees: FakeConfigTrees,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::config_tax_router(
        tax_rates,
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn tax_publish_writes_the_tax_node_onto_the_store_layer() {
    let tax_rates = FakeTaxRates::default();
    let class = TaxClassId::new(Ulid::from_u128(7));
    tax_rates.rows.lock().expect("lock").push((
        tenant(),
        TaxRateEntry::new(class, SalesChannel::DineIn, TaxRate::from_percent(10)),
    ));
    let config_trees = FakeConfigTrees::default();
    let router = tax_publish_app(provisioned_admin(), tax_rates, config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let published = router
        .oneshot(put_with_cookie(
            "/admin/config/tax",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
        ))
        .await
        .expect("route tax publish");
    assert_eq!(published.status(), StatusCode::OK);

    // The store's Store config layer (index 2) now carries the `tax` node — the serialized rate table.
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    let tax = &state.record.layers[2]["tax"];
    assert!(tax.is_array(), "the tax node is the serialized rate table");
    assert_eq!(tax.as_array().expect("array").len(), 1);
}

fn locale_publish_app(admin: FakeAdmin, config_trees: FakeConfigTrees) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::config_locale_router(
        &pos_cloud::countries::registry(),
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

fn store_profile_app(admin: FakeAdmin, config_trees: FakeConfigTrees) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::config_store_profile_router(
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
        Arc::new(pos_country::country_registry! { pos_country_jp::Japan }),
    ))
}

/// The store's registered identity reaches the `store_profile` node, and a registration number that
/// is not the shape the country issues is refused before it can reach an invoice
/// ([ADR-0106](../../../../docs/adr/0106-the-store-is-a-legal-person.md)).
#[tokio::test]
async fn a_store_profile_publish_writes_the_node_and_checks_the_registration_shape() {
    let config_trees = FakeConfigTrees::default();
    let router = store_profile_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let published = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/store-profile",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "legal_name": "Pizza 4P's Japan K.K.",
                "trading_name": "Pizza 4P's Ginza",
                "address_lines": ["中央区銀座 1-2-3", "   "],
                "tax_registration_number": "T1234567890123",
                "tax_registration_label": "登録番号",
                "country_code": "JP",
            }),
            &cookie,
        ))
        .await
        .expect("route the profile publish");
    assert_eq!(published.status(), StatusCode::OK);

    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    let profile = &state.record.layers[2]["store_profile"];
    assert_eq!(profile["trading_name"], "Pizza 4P's Ginza");
    assert_eq!(
        profile["address_lines"],
        serde_json::json!(["中央区銀座 1-2-3"]),
        "a blank line is dropped rather than printed on every receipt"
    );
    assert_eq!(profile["tax_registration_label"], "登録番号");

    // `T` and thirteen digits is Japan's shape; twelve is a typo that would otherwise reach a
    // qualified invoice, where it stops the buyer claiming input tax.
    let typo = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/store-profile",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "legal_name": "Pizza 4P's Japan K.K.",
                "tax_registration_number": "T123456789012",
                "country_code": "JP",
            }),
            &cookie,
        ))
        .await
        .expect("route the typo");
    assert_eq!(typo.status(), StatusCode::BAD_REQUEST);

    // A country this cloud does not carry stores the number unchecked rather than refusing: a fork
    // serving a market it has not written a pack for must still be able to provision a store.
    let unknown_country = router
        .oneshot(put_with_cookie(
            "/admin/config/store-profile",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "legal_name": "Pizza 4P's Bangkok",
                "tax_registration_number": "anything at all",
                "country_code": "TH",
            }),
            &cookie,
        ))
        .await
        .expect("route the unknown country");
    assert_eq!(unknown_country.status(), StatusCode::OK);
}

/// The node an edge actually receives says how many decimals the store's currency has
/// ([ADR-0134](../../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)).
///
/// Through the route rather than through `checked_locale_node`, because the value comes from the
/// compiled country registry the *router* is handed: a builder unit test passes just as happily
/// against a state wired with an empty map, and the wiring is half of what was missing.
///
/// The rupee is the case worth writing. Vietnam's exponent is zero and so is the edge session's
/// bootstrap, so a VND store cannot tell a published answer from an absent one — which is why the
/// gap survived two releases of work that depended on it.
#[tokio::test]
async fn a_locale_publish_tells_the_store_how_many_decimals_its_currency_has() {
    let config_trees = FakeConfigTrees::default();
    let router = locale_publish_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;

    let published = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/locale",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "country_code": "IN",
                "currency_code": "INR",
                "timezone": "Asia/Kolkata",
                "cutoff_hour": 4,
                "prices_include_tax": true,
            }),
            &cookie,
        ))
        .await
        .expect("route locale publish");
    assert_eq!(published.status(), StatusCode::OK);

    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    assert_eq!(
        state.record.layers[2]["locale"]["currency_exponent"], 2,
        "without this the edge keeps its bootstrap zero, `GET /api/locale` serves that zero as an \
         answer, and an Indian store prints `INR 26145` on a tax invoice"
    );
    assert_eq!(
        state.record.layers[2]["locale"]["default_retention_days"], 365,
        "the store's retention period travels on the same node and from the same registry \
         (ADR-0107), so the wiring that carries one carries both"
    );
}

#[tokio::test]
async fn locale_publish_validates_and_writes_the_locale_node() {
    let config_trees = FakeConfigTrees::default();
    let router = locale_publish_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let ok = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/locale",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                // Required since ADR-0114: this is the country a hosted store's region is compared
                // against, and it belongs beside the currency and timezone below rather than
                // anywhere new. Lower case in, upper case out.
                "country_code": "vn",
                "currency_code": "VND",
                "timezone": "Asia/Ho_Chi_Minh",
                "cutoff_hour": 4,
            }),
            &cookie,
        ))
        .await
        .expect("route locale publish");
    assert_eq!(ok.status(), StatusCode::OK);
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    assert_eq!(
        state.record.layers[2]["locale"]["timezone"],
        "Asia/Ho_Chi_Minh"
    );
    assert_eq!(state.record.layers[2]["locale"]["currency_code"], "VND");
    assert_eq!(
        state.record.layers[2]["locale"]["country_code"], "VN",
        "normalised on the way in, so the fleet console's comparison is an equality test"
    );

    // A malformed IANA timezone is a 400, validated before anything is written.
    let bad = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/locale",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "country_code": "VN",
                "currency_code": "VND",
                "timezone": "Nowhere/Nope",
                "cutoff_hour": 4,
            }),
            &cookie,
        ))
        .await
        .expect("route bad timezone");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // A publish that names no country is refused by name (ADR-0114). Absent and malformed answer
    // the *same* refusal, on the AIP-193 envelope: the field defaults to the empty string rather
    // than being a bare required field, because a missing field would otherwise be axum's own `422`
    // with a plain body, off the envelope every other `/admin` refusal uses.
    for country in [serde_json::Value::Null, serde_json::json!("Vietnam")] {
        let mut body = serde_json::json!({
            "tenant_id": tenant_ulid,
            "store_id": store_ulid,
            "currency_code": "VND",
            "timezone": "Asia/Ho_Chi_Minh",
            "cutoff_hour": 4,
        });
        if !country.is_null() {
            body["country_code"] = country.clone();
        }
        let refused = router
            .clone()
            .oneshot(put_with_cookie("/admin/config/locale", &body, &cookie))
            .await
            .expect("route the countryless publish");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "a store must say which country it is in: {country}"
        );
        let payload = json_body(refused).await;
        assert_eq!(payload["error"]["details"][0]["field"], "country_code");
    }
}

/// A store's event retention is published as its `retention` node and read back (ADR-0145), and a
/// figure outside 30..=3650 days is refused, naming the field.
#[tokio::test]
async fn a_store_s_event_retention_is_published_within_its_range() {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        trees.clone(),
        FakeWebhooks::default(),
    ))
    .merge(http::config_channels_router(
        trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ));
    let cookie = admin_cookie(&router).await;
    let publish = |days: u32| {
        put_with_cookie(
            "/admin/config/retention",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "event_log_days": days,
            }),
            &cookie,
        )
    };

    let published = router
        .clone()
        .oneshot(publish(45))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!(
                "/admin/config/retention?tenant_id={}&store_id={}",
                tenant().as_ulid(),
                store_id().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(json_body(read).await["event_log_days"], 45);

    for refused in [29, 3_651] {
        let response = router
            .clone()
            .oneshot(publish(refused))
            .await
            .expect("route the publish");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{refused} days");
        let body = json_body(response).await;
        assert_eq!(body["error"]["details"][0]["field"], "event_log_days");
        assert_eq!(body["error"]["details"][0]["reason"], "OUT_OF_RANGE");
    }
}

/// The cloud's bounds on `event_log_days` are the edge's: both take them from the `retention`
/// node's type in `pos-proto`, and this pins the numbers.
#[test]
fn the_cloud_bounds_event_retention_as_the_edge_does() {
    assert_eq!(http::EVENT_LOG_DAYS, 30..=3650);
}

/// The `retention` node the route publishes reads back through the type the edge reads it with,
/// at every figure the route takes, its bounds included; and a figure past what the type holds is
/// refused as one past the bounds is.
#[tokio::test]
async fn the_retention_node_reads_back_through_its_type() {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        trees.clone(),
        FakeWebhooks::default(),
    ))
    .merge(http::config_channels_router(
        trees.clone(),
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ));
    let cookie = admin_cookie(&router).await;
    let publish = |days: u32| {
        put_with_cookie(
            "/admin/config/retention",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "event_log_days": days,
            }),
            &cookie,
        )
    };

    for days in [30_u16, 45, 3650] {
        let published = router
            .clone()
            .oneshot(publish(u32::from(days)))
            .await
            .expect("route the publish");
        assert_eq!(published.status(), StatusCode::OK, "{days}");
        let tree = trees
            .load(tenant(), store_id())
            .await
            .expect("load")
            .expect("a tree");
        let node = &tree.record.layers[2]["retention"];
        let read = pos_proto::retention::PublishedRetention::read(node).expect("an object");
        assert_eq!(read.event_log_days(), Some(days));
        assert_eq!(&serde_json::to_value(read).expect("serialise"), node);
    }
    for refused in [70_000, u32::MAX] {
        let response = router
            .clone()
            .oneshot(publish(refused))
            .await
            .expect("route the publish");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{refused} days");
        let body = json_body(response).await;
        assert_eq!(
            body["error"]["message"],
            "event_log_days must be between 30 and 3650"
        );
        assert_eq!(body["error"]["details"][0]["reason"], "OUT_OF_RANGE");
    }
}

/// A store's `origins` node is authored here and refused here, by the very rule the edge applies
/// ([ADR-0111](../../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
///
/// The node is the only writer for a list the edge reads on the front of every request. Two things
/// have to hold for that to be worth anything: what the console saves is the shape the edge parses,
/// and what the edge would refuse the console refuses first. The second is the one that bites — a
/// cloud that stored `https://*.example.com` would leave an operator looking at a saved allow-list
/// their edge had silently thrown away whole.
#[tokio::test]
async fn a_store_publishes_the_origins_its_edge_will_answer() {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        trees.clone(),
        FakeWebhooks::default(),
    ))
    .merge(http::config_channels_router(
        trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ));
    let cookie = admin_cookie(&router).await;
    let scope = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
    });

    // A store that has never published one reads `null` — distinct from an empty list, which is a
    // person deliberately withdrawing every second origin.
    let before = router
        .clone()
        .oneshot(get_with_cookie(
            &format!(
                "/admin/config/origins?tenant_id={}&store_id={}",
                tenant().as_ulid(),
                store_id().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(before.status(), StatusCode::OK);
    assert!(json_body(before).await.is_null());

    // A valid publish stores the `{ "allowed": [...] }` shape the edge's `apply_origins` reads.
    let mut body = scope.clone();
    body["allowed"] = serde_json::json!(["https://shell.example.com", "http://localhost:1420"]);
    let published = router
        .clone()
        .oneshot(put_with_cookie("/admin/config/origins", &body, &cookie))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);

    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!(
                "/admin/config/origins?tenant_id={}&store_id={}",
                tenant().as_ulid(),
                store_id().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(
        json_body(read).await,
        serde_json::json!({
            "allowed": ["https://shell.example.com", "http://localhost:1420"]
        })
    );

    // A wildcard is refused at authoring time, naming the entry and the rule — not stored for an
    // edge to throw away in a log line nobody reads.
    let mut wildcard = scope.clone();
    wildcard["allowed"] = serde_json::json!(["https://*.example.com"]);
    let refused = router
        .clone()
        .oneshot(put_with_cookie("/admin/config/origins", &wildcard, &cookie))
        .await
        .expect("route the publish");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let message = json_body(refused).await["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("https://*.example.com"),
        "the refusal must name the entry the operator typed: {message}"
    );

    // And the refusal changed nothing: the previously published list still stands.
    let after = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/config/origins?tenant_id={}&store_id={}",
                tenant().as_ulid(),
                store_id().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(
        json_body(after).await,
        serde_json::json!({
            "allowed": ["https://shell.example.com", "http://localhost:1420"]
        })
    );
}
