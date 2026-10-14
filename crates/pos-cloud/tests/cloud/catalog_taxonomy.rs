// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The catalog's tax classes, categories, display taxonomy and layout buttons (ADR-0066).

use axum::http::StatusCode;
use tower::ServiceExt;

use crate::{
    FakeCatalog, admin_cookie, catalog_app, delete_with_cookie, etag_of, get_with_cookie,
    json_body, patch_with_etag, post_with_cookie, provisioned_admin, put_with_cookie,
    put_with_etag, ulid_text,
};

#[tokio::test]
async fn catalog_creates_lists_and_renames_a_tax_class() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // Created by name, the id minted server-side — an operator never types a tax-class ULID.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/tax-classes",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard 10%" }),
            &cookie,
        ))
        .await
        .expect("route create tax class");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Standard 10%");
    assert_eq!(created["status"], "active");
    let tax_class_id = created["tax_class_id"]
        .as_str()
        .expect("a tax class id")
        .to_owned();
    // The create hands back the version the record starts at (ADR-0094), so the edit below needs no
    // second round trip to learn it.
    let etag = created["etag"].as_str().expect("an etag").to_owned();

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-classes?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list tax classes");
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(json_body(listed).await.as_array().expect("array").len(), 1);

    // Rename + archive in one PATCH.
    let renamed = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({ "tenant_id": tenant, "name": "Alcohol", "status": "archived" }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename tax class");
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed = json_body(renamed).await;
    assert_eq!(renamed["name"], "Alcohol");
    assert_eq!(renamed["status"], "archived");

    // A PATCH to an unknown id is a 404, not a silent success — and not a 412 either: a
    // well-formed version for a row that does not exist is an absence, not a conflict (ADR-0094).
    let missing = router
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{}", ulid_text(999)),
            &serde_json::json!({ "tenant_id": tenant, "name": "Nope", "status": "active" }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename unknown tax class");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

/// The catalog's nine record-shaped entities are on the same conditional-write mechanism as the
/// registry (ADR-0094), and this is the case that matters: a second editor holding a stale copy is
/// refused, and the first editor's change is still there.
///
/// One entity stands for the nine because the seam, adapter and handler shape is identical across
/// them — what a per-entity copy of this test would prove is that the same code was pasted nine
/// times, not that nine behaviours are right.
#[tokio::test]
async fn a_stale_catalog_write_is_refused_and_the_first_edit_survives() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/tax-classes",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard 10%" }),
            &cookie,
        ))
        .await
        .expect("route create tax class");
    let created = json_body(created).await;
    let tax_class_id = created["tax_class_id"].as_str().expect("an id").to_owned();
    // Both editors load the record at the same version.
    let both_read = created["etag"].as_str().expect("an etag").to_owned();

    let first = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({ "tenant_id": tenant, "name": "Alcohol", "status": "active" }),
            &cookie,
            &both_read,
        ))
        .await
        .expect("route the first save");
    assert_eq!(first.status(), StatusCode::OK);

    let second = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({
                "tenant_id": tenant, "name": "Stale Overwrite", "status": "archived",
            }),
            &cookie,
            &both_read,
        ))
        .await
        .expect("route the second save");
    assert_eq!(second.status(), StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        json_body(second).await["error"]["status"],
        "VERSION_MISMATCH"
    );

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-classes?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the listing");
    let rows = json_body(listed).await;
    assert_eq!(
        rows[0]["name"], "Alcohol",
        "the refused write must not have applied: {rows}"
    );
    assert_eq!(
        rows[0]["status"], "active",
        "nor any other field it carried: {rows}"
    );
}

/// Archiving a taxonomy row that active items still point at is refused, and the refusal names how
/// many.
///
/// The failure this prevents is at the payment screen, not the console: `pos_core::billing` raises
/// `TaxRateNotConfigured` when no rate matches a line's tax class and channel, so an item left
/// pointing at a class nobody publishes a rate for is a shop that takes the order and cannot close
/// the bill. Nothing before that says a word — which is why the refusal has to be at the archive,
/// where an operator is still looking at the thing they are about to break.
#[tokio::test]
async fn archiving_a_tax_class_active_items_still_use_is_refused() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/tax-classes",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard 10%" }),
            &cookie,
        ))
        .await
        .expect("route create tax class");
    let created = json_body(created).await;
    let tax_class_id = created["tax_class_id"].as_str().expect("an id").to_owned();
    let class_etag = created["etag"].as_str().expect("an etag").to_owned();

    let item = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant, "name": "Margherita", "tax_class_id": tax_class_id,
            }),
            &cookie,
        ))
        .await
        .expect("route create item");
    let item = json_body(item).await;
    let item_id = item["menu_item_id"].as_str().expect("an id").to_owned();
    let item_etag = item["etag"].as_str().expect("an etag").to_owned();

    let refused = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({
                "tenant_id": tenant, "name": "Standard 10%", "status": "archived",
            }),
            &cookie,
            &class_etag,
        ))
        .await
        .expect("route the archive");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["status"], "UNPROCESSABLE");
    let message = body["error"]["message"].as_str().expect("a message");
    assert!(
        message.contains('1') && message.contains("item"),
        "the refusal names the size of the problem, not just that there is one: {message}"
    );

    // Renaming it is untouched — the guard is on the archive, not on the row.
    let renamed = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard", "status": "active" }),
            &cookie,
            &class_etag,
        ))
        .await
        .expect("route the rename");
    assert_eq!(renamed.status(), StatusCode::OK);
    let class_etag = json_body(renamed).await["etag"]
        .as_str()
        .expect("an etag")
        .to_owned();

    // Retire the item, and the class is free. An *archived* item's reference does not count: it is
    // not sold, so holding a tax class hostage to it would make this guard the thing operators
    // route around rather than the thing that protects them.
    let archived_item = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/items/{item_id}"),
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Margherita",
                "tax_class_id": tax_class_id,
                "status": "archived",
            }),
            &cookie,
            &item_etag,
        ))
        .await
        .expect("route archive the item");
    assert_eq!(archived_item.status(), StatusCode::OK);

    let allowed = router
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/tax-classes/{tax_class_id}"),
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard", "status": "archived" }),
            &cookie,
            &class_etag,
        ))
        .await
        .expect("route the second archive");
    assert_eq!(
        allowed.status(),
        StatusCode::OK,
        "with no active item referencing it, the archive goes through"
    );
}

#[tokio::test]
async fn catalog_item_taxonomy_categories_subcategories_and_item_linkage() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // A category, by name.
    let category = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/item-categories",
            &serde_json::json!({ "tenant_id": tenant, "name": "Pizza" }),
            &cookie,
        ))
        .await
        .expect("route create category");
    assert_eq!(category.status(), StatusCode::CREATED);
    let category_id = json_body(category).await["item_category_id"]
        .as_str()
        .expect("a category id")
        .to_owned();

    // A sub-category under it.
    let subcategory = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/item-subcategories",
            &serde_json::json!({ "tenant_id": tenant, "item_category_id": category_id, "name": "Thin crust" }),
            &cookie,
        ))
        .await
        .expect("route create subcategory");
    assert_eq!(subcategory.status(), StatusCode::CREATED);
    let subcategory = json_body(subcategory).await;
    assert_eq!(subcategory["item_category_id"], category_id);
    let subcategory_id = subcategory["item_subcategory_id"]
        .as_str()
        .expect("a sub-category id")
        .to_owned();

    // An item that references both — the linkage round-trips through create and list.
    let item = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Margherita",
                "tax_class_id": ulid_text(7),
                "item_category_id": category_id,
                "item_subcategory_id": subcategory_id,
            }),
            &cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(item.status(), StatusCode::CREATED);
    let item = json_body(item).await;
    assert_eq!(item["item_category_id"], category_id);
    assert_eq!(item["item_subcategory_id"], subcategory_id);

    let categories = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/item-categories?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list categories");
    assert_eq!(
        json_body(categories).await.as_array().expect("array").len(),
        1
    );

    let subcategories = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/item-subcategories?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list subcategories");
    assert_eq!(
        json_body(subcategories)
            .await
            .as_array()
            .expect("array")
            .len(),
        1
    );
}

/// A layout button's slot is the caller-supplied `(channel, item)` pair, so a second create at the
/// same slot is refused rather than relabelling and re-positioning the button already there, and a
/// relabel applies only at the version the list carried (ADR-0095).
///
/// The button needs a display category to sit under, which the round-trip test above creates; this
/// one takes the shortcut of an id that need not exist, because the layout compiler is forgiving of
/// a stale reference and these routes never resolve it.
#[tokio::test]
async fn a_layout_button_is_placed_once_and_relabelled_at_the_version_the_list_carried() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let item = ulid_text(500);
    let category = ulid_text(700);
    let row = format!("/admin/catalog/layout-buttons/SALES_CHANNEL_DINE_IN/{item}");
    let button = |label: &str| {
        serde_json::json!({
            "sales_channel": "SALES_CHANNEL_DINE_IN",
            "menu_item_id": item,
            "tenant_id": tenant,
            "display_category_id": category,
            "label": label,
            "grid_column": 0,
            "grid_row": 0,
            "sort": 0,
        })
    };

    let placed = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/layout-buttons",
            &button("Margherita"),
            &cookie,
        ))
        .await
        .expect("route create layout button");
    assert_eq!(placed.status(), StatusCode::CREATED);
    let at_create = etag_of(&placed);

    let again = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/layout-buttons",
            &button("Margherita (classic)"),
            &cookie,
        ))
        .await
        .expect("route the duplicate create");
    assert_eq!(again.status(), StatusCode::CONFLICT);

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/layout-buttons?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list layout buttons");
    let rows = json_body(listed).await;
    assert_eq!(rows.as_array().expect("array").len(), 1);
    assert_eq!(
        rows[0]["label"], "Margherita",
        "a refused create leaves the label it refused to overwrite alone"
    );
    assert_eq!(
        rows[0]["etag"].as_str().expect("the row carries an etag"),
        at_create,
        "and the list row carries the version a relabel has to send back"
    );

    let unconditional = router
        .clone()
        .oneshot(put_with_cookie(
            &row,
            &button("Margherita (classic)"),
            &cookie,
        ))
        .await
        .expect("route the unconditional relabel");
    assert_eq!(unconditional.status(), StatusCode::BAD_REQUEST);

    let relabelled = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &button("Margherita (classic)"),
            &cookie,
            &at_create,
        ))
        .await
        .expect("route relabel layout button");
    assert_eq!(relabelled.status(), StatusCode::OK);

    let replayed = router
        .oneshot(put_with_etag(
            &row,
            &button("Margherita (again)"),
            &cookie,
            &at_create,
        ))
        .await
        .expect("route the stale relabel");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn catalog_display_taxonomy_and_layout_buttons_round_trip() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let category = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/display-categories",
            &serde_json::json!({ "tenant_id": tenant, "name": "Summer specials" }),
            &cookie,
        ))
        .await
        .expect("route create display category");
    assert_eq!(category.status(), StatusCode::CREATED);
    let category_id = json_body(category).await["display_category_id"]
        .as_str()
        .expect("a display category id")
        .to_owned();

    let subcategory = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/display-subcategories",
            &serde_json::json!({ "tenant_id": tenant, "display_category_id": category_id, "name": "Cold" }),
            &cookie,
        ))
        .await
        .expect("route create display subcategory");
    assert_eq!(subcategory.status(), StatusCode::CREATED);

    // Upsert a button for an item onto the dine-in channel under the category.
    let item = ulid_text(500);
    let placed = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/layout-buttons",
            &serde_json::json!({
                "sales_channel": "SALES_CHANNEL_DINE_IN",
                "menu_item_id": item,
                "tenant_id": tenant,
                "display_category_id": category_id,
                "label": "Margherita",
                "grid_column": 0,
                "grid_row": 0,
                "sort": 0,
            }),
            &cookie,
        ))
        .await
        .expect("route create layout button");
    assert_eq!(placed.status(), StatusCode::CREATED);

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/layout-buttons?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list layout buttons");
    let buttons = json_body(listed).await;
    assert_eq!(buttons.as_array().expect("array").len(), 1);
    assert_eq!(buttons[0]["sales_channel"], "SALES_CHANNEL_DINE_IN");

    // Remove it — 204, then the list is empty.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!(
                "/admin/catalog/layout-buttons/SALES_CHANNEL_DINE_IN/{item}?tenant_id={tenant}"
            ),
            &cookie,
        ))
        .await
        .expect("route remove layout button");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let empty = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/layout-buttons?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list layout buttons again");
    assert_eq!(json_body(empty).await.as_array().expect("array").len(), 0);
}
