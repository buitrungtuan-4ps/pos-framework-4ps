// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An item's barcodes, PLU and open price over the authoring routes (ADR-0222 decision 1): checked
//! at the write, unique within the tenant's menu with both items named, and kept by an update that
//! does not mention them.

use axum::http::StatusCode;
use tower::ServiceExt;

use crate::{
    FakeCatalog, admin_cookie, catalog_app, etag_of, json_body, patch_with_etag, post_with_cookie,
    provisioned_admin, ulid_text,
};

#[tokio::test]
async fn an_items_codes_are_checked_unique_and_kept_by_an_update_that_omits_them() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let tax_class = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/tax-classes",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create tax class");
    let tax_class_id = json_body(tax_class).await["tax_class_id"]
        .as_str()
        .expect("a tax class id")
        .to_owned();
    let create = |name: &str, codes: serde_json::Value| {
        post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant, "name": name, "tax_class_id": tax_class_id, "codes": codes,
            }),
            &cookie,
        )
    };

    // A fake EAN-8 with its check digit, a PLU and an open price.
    let bread = router
        .clone()
        .oneshot(create(
            "Sourdough",
            serde_json::json!({
                "barcodes": ["96385074"], "plu": "4011",
                "open_price": {
                    "minimum": { "currency_code": "VND", "amount_minor": 10_000 },
                    "maximum": { "currency_code": "VND", "amount_minor": 90_000 },
                },
            }),
        ))
        .await
        .expect("route create");
    assert_eq!(bread.status(), StatusCode::CREATED);
    let etag = etag_of(&bread);
    let bread = json_body(bread).await;
    assert_eq!(bread["barcodes"][0], "96385074");
    assert_eq!(bread["plu"], "4011");

    let bad_digit = router
        .clone()
        .oneshot(create(
            "Rye",
            serde_json::json!({ "barcodes": ["96385075"] }),
        ))
        .await
        .expect("route create");
    assert_eq!(bad_digit.status(), StatusCode::BAD_REQUEST);

    let duplicate = router
        .clone()
        .oneshot(create("Rye", serde_json::json!({ "plu": "4011" })))
        .await
        .expect("route create");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let message = json_body(duplicate).await.to_string();
    assert!(
        message.contains("Sourdough") && message.contains("Rye"),
        "names both items: {message}"
    );

    // A rename from a console that knows nothing of codes keeps them.
    let item_id = bread["menu_item_id"].as_str().expect("an id").to_owned();
    let renamed = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/items/{item_id}"),
            &serde_json::json!({
                "tenant_id": tenant, "name": "Country sourdough", "tax_class_id": tax_class_id,
                "status": "active",
            }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route update");
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed = json_body(renamed).await;
    assert_eq!(renamed["plu"], "4011", "{renamed}");
    assert_eq!(renamed["barcodes"][0], "96385074");
}
