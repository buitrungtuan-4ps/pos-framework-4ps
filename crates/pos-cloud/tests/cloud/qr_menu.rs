// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /v1/qr/menu` (ADR-0224 decision 1): a guest with a signed table token reads the store's QR
//! menu in their language, under the intake's guardrails, and reading it never spends the table's
//! order budget.

use axum::http::StatusCode;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, FakeIntake, NOW_MS, TableTokenSecret, clock, get, json_body, known_menu_item,
    mint_table_token, order_store, post_json, qr_body, qr_router, qr_table, seed_versions, tenant,
};

/// A store with QR ordering on, a per-table limit of `limit`, and a one-item QR menu.
fn store_trees(limit: u32) -> FakeConfigTrees {
    let trees = FakeConfigTrees::default();
    seed_versions(
        &trees,
        tenant(),
        order_store(),
        &[(
            NOW_MS,
            serde_json::json!({
                "qr_ordering_enabled": true,
                "qr": { "per_table_limit": limit, "rate_window_secs": 60 },
                "locale": { "display_language": "vi" },
                "menu": { "channels": [{
                    "sales_channel": "SALES_CHANNEL_QR",
                    "catalog": { "items": [{
                        "menu_item_id": "01HZ0000000000000000000001",
                        "display_name": "Margherita",
                        "display_name_translations": { "vi": "Bánh Margherita" },
                        "unit_price": { "currency_code": "VND", "amount_minor": 189_000 },
                        "tax_class_id": "01HZ00000000000000000000T1"
                    }] }
                }] }
            }),
        )],
    );
    trees
}

fn menu_uri(token: &str, lang: &str) -> String {
    format!("/v1/qr/menu?table_token={token}&lang={lang}")
}

#[tokio::test]
async fn a_guest_reads_the_qr_menu_with_a_signed_table_token() {
    let secret = TableTokenSecret::new("qr-endpoint-secret");
    let token = mint_table_token(&secret, tenant(), order_store(), qr_table());
    let router = qr_router(secret, FakeIntake::new(), store_trees(10), clock());
    let response = router
        .oneshot(get(&menu_uri(&token, "vi"), None))
        .await
        .expect("route the menu read");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["language"], "vi", "{body}");
    assert_eq!(body["items"][0]["name"], "Bánh Margherita", "{body}");
    assert_eq!(body["items"][0]["unit_price"]["amount_minor"], 189_000);
    assert!(body["items"][0].get("tax_class_id").is_none(), "{body}");
}

#[tokio::test]
async fn a_forged_token_or_a_store_without_qr_ordering_reads_nothing() {
    let secret = TableTokenSecret::new("qr-endpoint-secret");
    let forged = mint_table_token(
        &TableTokenSecret::new("another-secret"),
        tenant(),
        order_store(),
        qr_table(),
    );
    let response = qr_router(secret.clone(), FakeIntake::new(), store_trees(10), clock())
        .oneshot(get(&menu_uri(&forged, "en"), None))
        .await
        .expect("route the menu read");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = json_body(response).await;
    assert!(body.get("items").is_none(), "{body}");

    let token = mint_table_token(&secret, tenant(), order_store(), qr_table());
    let off = FakeConfigTrees::default();
    seed_versions(
        &off,
        tenant(),
        order_store(),
        &[(NOW_MS, serde_json::json!({ "qr_ordering_enabled": false }))],
    );
    let response = qr_router(secret, FakeIntake::new(), off, clock())
        .oneshot(get(&menu_uri(&token, "en"), None))
        .await
        .expect("route the menu read");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn menu_reads_are_rate_limited_apart_from_the_tables_orders() {
    let secret = TableTokenSecret::new("qr-endpoint-secret");
    let token = mint_table_token(&secret, tenant(), order_store(), qr_table());
    let router = qr_router(secret, FakeIntake::new(), store_trees(2), clock());
    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(get(&menu_uri(&token, "en"), None))
            .await
            .expect("route the menu read");
        assert_eq!(response.status(), StatusCode::OK);
    }
    let response = router
        .clone()
        .oneshot(get(&menu_uri(&token, "en"), None))
        .await
        .expect("route the menu read");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // The table's order budget is untouched by its reads.
    let (known, _price) = known_menu_item();
    let response = router
        .oneshot(post_json(
            "/v1/qr/orders",
            &qr_body(&token, "qr-after-reads", known),
        ))
        .await
        .expect("route the QR order");
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn the_guest_page_is_served_with_a_policy_that_runs_only_its_own_script() {
    let secret = TableTokenSecret::new("qr-endpoint-secret");
    let router = qr_router(secret, FakeIntake::new(), store_trees(10), clock());
    for (uri, kind) in [
        ("/qr/", "text/html"),
        ("/qr/app.js", "text/javascript"),
        ("/qr/style.css", "text/css"),
        ("/qr/i18n/vi.json", "application/json"),
    ] {
        let response = router
            .clone()
            .oneshot(get(uri, None))
            .await
            .expect("route the page");
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        assert!(header("content-type").starts_with(kind), "{uri}");
        assert!(
            header("content-security-policy").contains("script-src 'self'"),
            "{uri}"
        );
        assert_eq!(header("referrer-policy"), "no-referrer", "{uri}");
        assert_eq!(header("x-content-type-options"), "nosniff", "{uri}");
    }
    for uri in ["/qr/i18n/fr.json", "/qr/i18n/..%2Fapp.js"] {
        let response = router
            .clone()
            .oneshot(get(uri, None))
            .await
            .expect("route the page");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn a_guest_order_is_refused_unless_it_is_shaped_as_the_page_sends_it() {
    let secret = TableTokenSecret::new("qr-endpoint-secret");
    let token = mint_table_token(&secret, tenant(), order_store(), qr_table());
    let (known, _price) = known_menu_item();
    let router = qr_router(secret, FakeIntake::new(), store_trees(100), clock());
    let line = |quantity: i64| serde_json::json!({ "menu_item_id": known.to_string(), "quantity_milli": quantity });
    let order = |lines: Vec<serde_json::Value>| {
        serde_json::json!({
            "table_token": token,
            "external_reference": "qr-shape",
            "lines": lines,
            "placed_at_ms": NOW_MS,
        })
    };
    let mut noted = line(1_000);
    noted["note"] = serde_json::json!("call me on my phone");
    for body in [
        order(Vec::new()),
        order(vec![line(1_000); 31]),
        order(vec![line(1_500)]),
        order(vec![line(100_000)]),
        order(vec![noted]),
    ] {
        let response = router
            .clone()
            .oneshot(post_json("/v1/qr/orders", &body))
            .await
            .expect("route the QR order");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
    }
    let response = router
        .oneshot(post_json("/v1/qr/orders", &order(vec![line(2_000)])))
        .await
        .expect("route the QR order");
    assert_eq!(response.status(), StatusCode::CREATED);
}
