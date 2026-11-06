// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `receipt` node's templates, written as the setting `receipt.templates` (ADR-0207): its
//! keys come from the tenant's grid with English, and a section a country requires stays on.

use axum::http::StatusCode;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{
    admin_cookie, json_body, put_number, put_with_cookie, settings_app, settings_stores,
    tenant_layer_node,
};

const KEY: &str = "receipt.templates";

/// Writes `value` at the tenant, and answers the status and the body.
async fn write(router: &axum::Router, cookie: &str, value: &Value) -> (StatusCode, Value) {
    let answer = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &put_number(KEY, value),
            cookie,
        ))
        .await
        .expect("route the write");
    let status = answer.status();
    (status, json_body(answer).await)
}

/// The reason a `400` names for the value.
fn reason(body: &Value) -> String {
    body.to_string()
}

#[tokio::test]
async fn a_template_with_grid_keys_reaches_every_store_on_its_receipt_node() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let template = json!({
        "base": { "footer_keys": ["receipt.footer.thanks"], "sections": { "payments": false } },
        "channels": {
            "SALES_CHANNEL_TAKEAWAY": {
                "footer_keys": ["receipt.footer.thanks"],
                "person_copy": ["PERSON_COPY_RECORD_SLIP"]
            }
        }
    });
    let (status, _) = write(&router, &cookie, &template).await;
    assert_eq!(status, StatusCode::OK);
    for store_id in settings_stores() {
        assert_eq!(
            tenant_layer_node(&config_trees, store_id, "receipt").await,
            Some(json!({ "templates": template })),
            "the node the edge reads"
        );
    }
}

#[tokio::test]
async fn a_key_the_grid_has_no_english_for_is_refused_and_nothing_is_written() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    for key in ["receipt.footer.untranslated", "receipt.footer.nowhere"] {
        let (status, body) = write(
            &router,
            &cookie,
            &json!({ "base": { "header_keys": [key] } }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key}");
        assert!(reason(&body).contains("UNKNOWN_TRANSLATION_KEY"), "{body}");
    }
    let [first, _, _] = settings_stores();
    assert_eq!(
        tenant_layer_node(&config_trees, first, "receipt").await,
        None
    );
}

#[tokio::test]
async fn a_template_cannot_turn_off_a_section_a_country_requires() {
    let (router, _, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    // Vietnam, Japan and India each require the modifiers (their `receipt_required_sections`).
    let (status, body) = write(
        &router,
        &cookie,
        &json!({ "channels": { "SALES_CHANNEL_DELIVERY": { "sections": { "modifiers": false } } } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(reason(&body).contains("SECTION_REQUIRED"), "{body}");
}

#[tokio::test]
async fn free_text_and_values_over_the_limits_are_refused() {
    let (router, _, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    for (value, expected) in [
        (
            json!({ "base": { "footer_text": "Thank you" } }),
            "INVALID_VALUE",
        ),
        (json!("Thank you"), "INVALID_VALUE"),
        (
            json!({ "base": { "footer_keys": vec!["receipt.footer.thanks"; 5] } }),
            "OUT_OF_RANGE",
        ),
    ] {
        let (status, body) = write(&router, &cookie, &value).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert!(reason(&body).contains(expected), "{value}: {body}");
    }
}
