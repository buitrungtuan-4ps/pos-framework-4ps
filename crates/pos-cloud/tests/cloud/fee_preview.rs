// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/fees/preview`: a rule shown on a sample bill before it is written (ADR-0159).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::config_tree::ConfigTreeStore;
use pos_cloud::fees::InMemoryFeeRules;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::StoreId;
use tower::ServiceExt;

use super::fees::{
    fee_catalog, fee_item, fees_app, fees_registry, fees_stores, percent_fee, seed_fee_stores,
    store_fees, tenant_scope_id,
};
use crate::{
    FakeConfigTrees, FakeKeys, FakeRollups, FakeSettings, FakeStoreGroups, FakeWebhooks,
    admin_cookie, app_all, clock, get_with_cookie, json_body, post_with_cookie, provisioned_admin,
    put_with_cookie, role_session_cookie, tenant,
};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one sample bill read four ways against the same store — with the rule, without it, \
              with a line its menu does not sell, and at a store with nothing published — and by \
              a role that may not read prices; split, each would rebuild the same fixture"
)]
async fn a_sample_bill_shows_a_rule_before_it_is_written_and_writes_nothing() {
    let (router, config_trees, audit, admin) = fees_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, third] = fees_stores();
    let preview = |store_id: StoreId, rules: &serde_json::Value, lines: &serde_json::Value| {
        serde_json::json!({
            "tenant_id": tenant_scope_id(),
            "store_id": store_id.to_string(),
            "rules": rules,
            "lines": lines,
        })
    };
    let sample = serde_json::json!([
        { "menu_item_id": fee_item(1).to_string(), "quantity": 2 },
        { "menu_item_id": fee_item(3).to_string(), "quantity": 1 },
    ]);
    let service = serde_json::json!([{
        "scope": "FEE_SCOPE_TENANT",
        "scope_id": tenant_scope_id(),
        "rule": percent_fee("SERVICE", 5),
    }]);

    let previewed = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees/preview",
            &preview(first, &service, &sample),
            &cookie,
        ))
        .await
        .expect("route the preview");
    assert_eq!(previewed.status(), StatusCode::OK);
    let bill = json_body(previewed).await;
    let minor = |value: &serde_json::Value| value["amount_minor"].as_i64();
    assert_eq!(minor(&bill["lines"][0]["line_total"]), Some(190_000));
    assert_eq!(bill["lines"][0]["quantity"], 2);
    assert_eq!(minor(&bill["subtotal"]), Some(220_000));
    assert_eq!(bill["fee_lines"][0]["code"], "SERVICE");
    assert_eq!(minor(&bill["fee_lines"][0]["amount"]), Some(11_000));
    assert_eq!(minor(&bill["fee_lines"][0]["tax"]), Some(880));
    assert_eq!(bill["tax_lines"][0]["rate_basis_points"], 800);
    assert_eq!(minor(&bill["tax_total"]), Some(18_480));
    assert_eq!(minor(&bill["total_due"]), Some(249_480));

    // A preview writes nothing and publishes nothing.
    assert!(store_fees(&config_trees, first).await.is_none());
    let listed = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/fees?tenant_id={}", tenant().as_ulid()),
                &cookie,
            ))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(listed["rules"], serde_json::json!([]));
    assert!(audit.entries.lock().expect("lock").is_empty());

    // Without the rule, the same lines carry no fee.
    let plain = json_body(
        router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/fees/preview",
                &preview(first, &serde_json::json!([]), &sample),
                &cookie,
            ))
            .await
            .expect("route the preview"),
    )
    .await;
    assert_eq!(plain["fee_lines"], serde_json::json!([]));
    assert_eq!(minor(&plain["total_due"]), Some(237_600));

    // A line the store's menu does not sell, and a store with nothing published to price from.
    for (store_id, lines, field, reason) in [
        (
            first,
            serde_json::json!([{ "menu_item_id": fee_item(2).to_string(), "quantity": 1 }]),
            "lines",
            "NOT_ON_MENU",
        ),
        (third, sample.clone(), "store_id", "NOT_PUBLISHED"),
    ] {
        let refused = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/fees/preview",
                &preview(store_id, &service, &lines),
                &cookie,
            ))
            .await
            .expect("route the preview");
        assert_eq!(
            refused.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{reason}"
        );
        let body = json_body(refused).await;
        assert_eq!(body["error"]["details"][0]["field"], field);
        assert_eq!(body["error"]["details"][0]["reason"], reason);
    }

    // A preview shows prices, so it needs what reading a rule needs.
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let by_ops = router
        .oneshot(post_with_cookie(
            "/admin/fees/preview",
            &preview(first, &service, &sample),
            &ops,
        ))
        .await
        .expect("route the preview");
    assert_eq!(by_ops.status(), StatusCode::FORBIDDEN);
}

/// The fee routes, the settings routes and the locale publish over one config tree, as the cloud
/// serves them, so what a setting writes is what a sample bill reads.
fn fees_and_settings_app() -> (axum::Router, FakeConfigTrees) {
    let admin = provisioned_admin();
    let config_trees = FakeConfigTrees::default();
    let registry = fees_registry();
    seed_fee_stores(&config_trees);
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    let router = http::router(app)
        .merge(http::fees_router(
            InMemoryFeeRules::new(),
            registry.clone(),
            fee_catalog(),
            config_trees.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::settings_router(
            FakeSettings::default(),
            registry,
            FakeStoreGroups::default(),
            config_trees.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::config_locale_router(
            &pos_cloud::countries::registry(),
            config_trees.clone(),
            admin,
            clock(),
            Arc::new(NoopAuditRecorder),
        ));
    (router, config_trees)
}

/// The fee, the tax and the total of a sample bill of one Margherita at a 5.05 % fee.
async fn sampled(router: &axum::Router, cookie: &str, store_id: StoreId) -> [Option<i64>; 3] {
    let body = serde_json::json!({
        "tenant_id": tenant_scope_id(),
        "store_id": store_id.to_string(),
        "rules": [{
            "scope": "FEE_SCOPE_TENANT",
            "scope_id": tenant_scope_id(),
            "rule": {
                "code": "SERVICE",
                "display_name": "Service charge",
                "kind": "FEE_KIND_PERCENT",
                "rate": { "numerator": 505, "denominator": 10_000 },
            },
        }],
        "lines": [{ "menu_item_id": fee_item(1).to_string(), "quantity": 1 }],
    });
    let previewed = router
        .clone()
        .oneshot(post_with_cookie("/admin/fees/preview", &body, cookie))
        .await
        .expect("route the preview");
    assert_eq!(previewed.status(), StatusCode::OK);
    let bill = json_body(previewed).await;
    [
        bill["fee_lines"][0]["amount"]["amount_minor"].as_i64(),
        bill["tax_total"]["amount_minor"].as_i64(),
        bill["total_due"]["amount_minor"].as_i64(),
    ]
}

/// A store's tax rounding (ADR-0160) is a setting on its `locale` node, written on its Tenant
/// layer, while a locale publish writes the country's fields on its Store layer. The node the
/// store receives carries both, whichever was written last, and a sample bill at the store rounds
/// its fee and its tax as the store's edge will: 5.05 % of 95,000 is 4,797.5, and 8 % of the line
/// and the fee is 7,983.84 at half-up or 7,983.76 rounded down.
#[tokio::test]
async fn a_store_set_to_round_tax_down_keeps_its_country_and_is_shown_the_tax_it_will_charge() {
    let (router, config_trees) = fees_and_settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = fees_stores();
    assert_eq!(
        sampled(&router, &cookie, first).await,
        [Some(4_798), Some(7_984), Some(107_782)],
        "half-up while the store sets nothing"
    );

    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &serde_json::json!({
                "tenant_id": tenant_scope_id(),
                "setting_key": "locale.tax_rounding",
                "scope": "SETTING_SCOPE_STORE",
                "scope_id": first.to_string(),
                "value": "TAX_ROUNDING_DOWN",
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
        tree.record.layers[0]["locale"],
        serde_json::json!({ "tax_rounding": "TAX_ROUNDING_DOWN" }),
        "the setting alone, on the Tenant layer"
    );
    assert_eq!(
        tree.record.layers[2]["locale"],
        serde_json::json!({ "currency_code": "VND" }),
        "the store's own locale is not touched"
    );
    let down = [Some(4_797), Some(7_983), Some(107_780)];
    assert_eq!(sampled(&router, &cookie, first).await, down);

    // The store's locale published again, as when its country is set: the setting stays.
    let published = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/locale",
            &serde_json::json!({
                "tenant_id": tenant_scope_id(),
                "store_id": first.to_string(),
                "country_code": "VN",
                "currency_code": "VND",
                "timezone": "Asia/Ho_Chi_Minh",
                "cutoff_hour": 4,
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let effective = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!(
                    "/admin/stores/{}/config?tenant_id={}",
                    first.as_ulid(),
                    tenant().as_ulid()
                ),
                &cookie,
            ))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(effective["locale"]["tax_rounding"], "TAX_ROUNDING_DOWN");
    assert_eq!(effective["locale"]["country_code"], "VN");
    assert_eq!(effective["locale"]["timezone"], "Asia/Ho_Chi_Minh");
    assert_eq!(sampled(&router, &cookie, first).await, down);
}

// --- pinned before `admin_preview_fees` is split (ADR-0170 decision 7) ---------------------------

/// A preview body for the brand's first store: `extra` is merged over one sample line and no rules.
fn preview_body(extra: &serde_json::Value) -> serde_json::Value {
    let [first, _, _] = fees_stores();
    let mut body = serde_json::json!({
        "tenant_id": tenant_scope_id(),
        "store_id": first.to_string(),
        "rules": [],
        "lines": [{ "menu_item_id": fee_item(1).to_string(), "quantity": 2 }],
    });
    if let (Some(fields), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            fields.insert(key.clone(), value.clone());
        }
    }
    body
}

/// The status and the first detail's field of a preview of `body`.
async fn previewed(
    router: &axum::Router,
    cookie: &str,
    body: &serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(post_with_cookie("/admin/fees/preview", body, cookie))
        .await
        .expect("route the preview");
    let status = response.status();
    (status, json_body(response).await)
}

fn tenant_rule(rule: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "scope": "FEE_SCOPE_TENANT", "scope_id": tenant_scope_id(), "rule": rule })
}

#[tokio::test]
async fn a_preview_refuses_its_request_field_by_field_in_the_order_it_reads_them() {
    let (router, _, _, _) = fees_app();
    let cookie = admin_cookie(&router).await;
    let too_many = serde_json::Value::Array(vec![tenant_rule(&percent_fee("SERVICE", 5)); 21]);
    for (extra, status, field) in [
        (
            serde_json::json!({ "tenant_id": "not-a-ulid", "sales_channel": "SALES_CHANNEL_UNSPECIFIED" }),
            StatusCode::BAD_REQUEST,
            "tenant_id",
        ),
        (
            serde_json::json!({ "sales_channel": "SALES_CHANNEL_UNSPECIFIED", "rules": too_many, "lines": [] }),
            StatusCode::BAD_REQUEST,
            "sales_channel",
        ),
        (
            serde_json::json!({ "rules": too_many, "lines": [] }),
            StatusCode::BAD_REQUEST,
            "rules",
        ),
        (
            serde_json::json!({ "lines": [] }),
            StatusCode::BAD_REQUEST,
            "lines",
        ),
    ] {
        let (got, body) = previewed(&router, &cookie, &preview_body(&extra)).await;
        assert_eq!(got, status, "{extra}: {body}");
        assert_eq!(
            body["error"]["details"][0]["field"], field,
            "{extra}: {body}"
        );
    }

    let (got, _) = previewed(
        &router,
        &cookie,
        &preview_body(&serde_json::json!({ "store_id": StoreId::new(pos_proto::ulid::Ulid::from_u128(0x7B99)).to_string() })),
    )
    .await;
    assert_eq!(
        got,
        StatusCode::NOT_FOUND,
        "a store the tenant does not have"
    );

    let (got, _) = previewed(
        &router,
        &cookie,
        &preview_body(&serde_json::json!({ "rules": [{ "scope": "FEE_SCOPE_GALAXY", "scope_id": tenant_scope_id(), "rule": percent_fee("SERVICE", 5) }] })),
    )
    .await;
    assert_eq!(got, StatusCode::BAD_REQUEST, "a rule a write would refuse");
}

#[tokio::test]
async fn a_preview_prices_on_the_channel_it_names() {
    let (router, _, _, _) = fees_app();
    let cookie = admin_cookie(&router).await;
    let (got, body) = previewed(
        &router,
        &cookie,
        &preview_body(&serde_json::json!({ "sales_channel": "SALES_CHANNEL_DINE_IN" })),
    )
    .await;
    assert_eq!(got, StatusCode::OK);
    assert_eq!(body["sales_channel"], "SALES_CHANNEL_DINE_IN");

    // The store's menu sells nothing for takeaway, so the same line is not on it there.
    let (got, body) = previewed(
        &router,
        &cookie,
        &preview_body(&serde_json::json!({ "sales_channel": "SALES_CHANNEL_TAKEAWAY" })),
    )
    .await;
    assert_eq!(got, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"][0]["reason"], "NOT_ON_MENU");
}

#[tokio::test]
async fn a_previewed_rule_takes_the_place_of_the_written_rule_at_its_slot() {
    let (router, _, _, _) = fees_app();
    let cookie = admin_cookie(&router).await;
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees",
            &serde_json::json!({
                "tenant_id": tenant_scope_id(),
                "scope": "FEE_SCOPE_TENANT",
                "scope_id": tenant_scope_id(),
                "rule": percent_fee("SERVICE", 10),
            }),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(created.status(), StatusCode::CREATED);
    let fee_id = json_body(created).await["fee_id"]
        .as_str()
        .expect("a fee id")
        .to_owned();
    let amounts = |body: &serde_json::Value| -> Vec<Option<i64>> {
        body["fee_lines"]
            .as_array()
            .expect("fee lines")
            .iter()
            .map(|line| line["amount"]["amount_minor"].as_i64())
            .collect()
    };

    // What is written applies: 10 % of 190,000.
    let (got, written) = previewed(&router, &cookie, &preview_body(&serde_json::json!({}))).await;
    assert_eq!(got, StatusCode::OK);
    assert_eq!(amounts(&written), [Some(19_000)]);

    // The same fee at 5 % replaces it rather than joining it.
    let mut five = percent_fee("SERVICE", 5);
    if let Some(fields) = five.as_object_mut() {
        fields.insert("fee_id".to_owned(), serde_json::json!(fee_id));
    }
    let (got, replaced) = previewed(
        &router,
        &cookie,
        &preview_body(&serde_json::json!({ "rules": [tenant_rule(&five)] })),
    )
    .await;
    assert_eq!(got, StatusCode::OK);
    assert_eq!(amounts(&replaced), [Some(9_500)]);
}

#[tokio::test]
async fn a_rule_the_store_cannot_apply_is_refused_with_its_fault() {
    let (router, _, _, _) = fees_app();
    let cookie = admin_cookie(&router).await;
    let yen = serde_json::json!({
        "code": "DELIVERY",
        "display_name": "Delivery",
        "kind": "FEE_KIND_AMOUNT_PER_BILL",
        "amount": { "currency_code": "JPY", "amount_minor": 300 },
    });
    let (got, body) = previewed(
        &router,
        &cookie,
        &preview_body(&serde_json::json!({ "rules": [tenant_rule(&yen)] })),
    )
    .await;
    assert_eq!(got, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"]["details"],
        serde_json::json!([{ "field": "rules", "reason": "CURRENCY_MISMATCH" }])
    );
    let message = body["error"]["message"].as_str().expect("a message");
    assert!(
        message.starts_with("fee ")
            && message.ends_with(" cannot apply at the store: CURRENCY_MISMATCH"),
        "{message}"
    );
}
