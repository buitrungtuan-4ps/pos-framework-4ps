// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The inventory authoring routes (`/admin/inventory/*`, ADR-0079, ADR-0095), on the fakes.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeInventory, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie,
    app_all, clock, delete_with_cookie, etag_of, get_with_cookie, json_body, post_with_cookie,
    provisioned_admin, put_with_cookie, put_with_etag, ulid_text,
};

/// The admin surface plus the inventory router, on fakes.
fn inventory_app(admin: FakeAdmin, inventory: FakeInventory) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::inventory_router(
        inventory,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// A recipe write body, and the `POST` variant that also names the item it makes.
fn recipe_bodies(
    tenant: &str,
    item: &str,
    threshold: i64,
) -> (serde_json::Value, serde_json::Value) {
    let write =
        serde_json::json!({ "tenant_id": tenant, "lines": [], "auto_86_threshold": threshold });
    let mut create = write.clone();
    create["item_id"] = serde_json::Value::String(item.to_owned());
    (create, write)
}

/// One record of each inventory kind, with the id and the version each create handed back.
struct SeededInventory {
    ingredient: String,
    ingredient_etag: String,
    recipe_etag: String,
    supplier: String,
    supplier_etag: String,
}

/// Seeds one ingredient, one recipe and one supplier *through the routes*, so the ids and versions
/// are the ones a console would actually hold when it goes on to read or edit them.
async fn seed_one_of_each_inventory_kind(
    router: &axum::Router,
    cookie: &str,
    tenant: &str,
    item: &str,
) -> SeededInventory {
    // Seed one of each kind, keeping the version each create handed back: the `PUT`s need it.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/ingredients",
            &serde_json::json!({ "tenant_id": tenant, "name": "Bột mì", "unit": "UNIT_OF_MEASURE_GRAM" }),
            cookie,
        ))
        .await
        .expect("route create ingredient");
    assert_eq!(created.status(), StatusCode::CREATED);
    let ingredient_etag = etag_of(&created);
    let ingredient = json_body(created).await["id"]
        .as_str()
        .expect("an ingredient id")
        .to_owned();

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(tenant, item, 2).0,
            cookie,
        ))
        .await
        .expect("route create recipe");
    assert_eq!(created.status(), StatusCode::CREATED);
    let recipe_etag = etag_of(&created);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/suppliers",
            &serde_json::json!({ "tenant_id": tenant, "name": "Chợ Bến Thành" }),
            cookie,
        ))
        .await
        .expect("route create supplier");
    assert_eq!(created.status(), StatusCode::CREATED);
    let supplier_etag = etag_of(&created);
    let supplier = json_body(created).await["id"]
        .as_str()
        .expect("a supplier id")
        .to_owned();

    SeededInventory {
        ingredient,
        ingredient_etag,
        recipe_etag,
        supplier,
        supplier_etag,
    }
}

/// A collection route reads the collection — the other half of the read-one claim.
///
/// Without this the sibling test below proves nothing: "no `list_*` in the log" would also hold if
/// the log were broken, or if `list_ingredients` never recorded anything. This pins the log's
/// positive case, so the negative one means what it says.
#[tokio::test]
async fn the_inventory_collection_routes_read_the_collection() {
    let inventory = FakeInventory::default();
    let router = inventory_app(provisioned_admin(), inventory.clone());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    inventory.taken_reads();
    for collection in [
        "/admin/inventory/ingredients",
        "/admin/inventory/recipes",
        "/admin/inventory/suppliers",
    ] {
        let listed = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("{collection}?tenant_id={tenant}"),
                &cookie,
            ))
            .await
            .expect("route the list");
        assert_eq!(listed.status(), StatusCode::OK);
    }
    assert_eq!(
        inventory.taken_reads(),
        vec!["list_ingredients", "list_recipes", "list_suppliers"],
        "each collection route reads its own collection, and the log records it",
    );
}

/// Every `/admin/inventory` route that acts on one record reads that one record, never the list.
///
/// The behaviour is identical either way, which is exactly why this test exists: before the
/// read-one seam these nine handlers called `list_*` and scanned the result in the cloud, and no
/// assertion in this file could tell the difference. The cost was real though — a read, an edit or
/// a delete of one ingredient carried the tenant's entire ingredient list out of the database, and
/// grew with the catalogue.
///
/// Asserted as "the log holds no `list_*`" rather than as an exact call sequence, so adding a
/// second single-row read to a handler later does not have to be re-encoded here; a regression to
/// scanning does.
///
/// This is also the first route test to reach the ingredient and supplier handlers at all — before
/// it, only the recipe routes had one.
#[tokio::test]
async fn every_single_record_inventory_route_reads_one_row_and_never_the_whole_list() {
    let inventory = FakeInventory::default();
    let router = inventory_app(provisioned_admin(), inventory.clone());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let item = ulid_text(500);

    let SeededInventory {
        ingredient,
        ingredient_etag,
        recipe_etag,
        supplier,
        supplier_etag,
    } = seed_one_of_each_inventory_kind(&router, &cookie, &tenant, &item).await;

    // The nine single-record routes: read, edit, delete, for each of the three kinds.
    let ingredient_body = serde_json::json!({ "tenant_id": tenant, "name": "Bột mì số 13", "unit": "UNIT_OF_MEASURE_GRAM" });
    let supplier_body = serde_json::json!({ "tenant_id": tenant, "name": "Chợ Bình Tây" });
    let recipe_body = recipe_bodies(&tenant, &item, 5).1;
    let calls: Vec<(&str, String, Option<&serde_json::Value>, Option<&str>)> = vec![
        (
            "GET",
            format!("/admin/inventory/ingredients/{ingredient}?tenant_id={tenant}"),
            None,
            None,
        ),
        (
            "PUT",
            format!("/admin/inventory/ingredients/{ingredient}"),
            Some(&ingredient_body),
            Some(ingredient_etag.as_str()),
        ),
        (
            "DELETE",
            format!("/admin/inventory/ingredients/{ingredient}?tenant_id={tenant}"),
            None,
            None,
        ),
        (
            "GET",
            format!("/admin/inventory/recipes/{item}?tenant_id={tenant}"),
            None,
            None,
        ),
        (
            "PUT",
            format!("/admin/inventory/recipes/{item}"),
            Some(&recipe_body),
            Some(recipe_etag.as_str()),
        ),
        (
            "DELETE",
            format!("/admin/inventory/recipes/{item}?tenant_id={tenant}"),
            None,
            None,
        ),
        (
            "GET",
            format!("/admin/inventory/suppliers/{supplier}?tenant_id={tenant}"),
            None,
            None,
        ),
        (
            "PUT",
            format!("/admin/inventory/suppliers/{supplier}"),
            Some(&supplier_body),
            Some(supplier_etag.as_str()),
        ),
        (
            "DELETE",
            format!("/admin/inventory/suppliers/{supplier}?tenant_id={tenant}"),
            None,
            None,
        ),
    ];
    for (method, uri, body, etag) in calls {
        let request = match (body, etag) {
            (Some(body), Some(etag)) => put_with_etag(&uri, body, &cookie, etag),
            (Some(body), None) => post_with_cookie(&uri, body, &cookie),
            (None, _) if method == "DELETE" => delete_with_cookie(&uri, &cookie),
            (None, _) => get_with_cookie(&uri, &cookie),
        };
        let answered = router
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|_ignored| panic!("route {method} {uri}"));
        assert!(
            answered.status().is_success(),
            "{method} {uri} should have succeeded; got {}",
            answered.status(),
        );
        let reads = inventory.taken_reads();
        assert!(
            !reads.iter().any(|read| read.starts_with("list_")),
            "{method} {uri} acts on one record and must not read the whole list; it read {reads:?}",
        );
        assert!(
            !reads.is_empty(),
            "{method} {uri} should have read the record it acts on; it read nothing",
        );
    }
}

/// A recipe's key is the item it makes, and that id comes from the caller — so this was the seam
/// that lost data: the `PUT` was a create-or-replace, and "add a recipe" for an item that already
/// had one silently discarded its bill of materials (ADR-0095).
#[tokio::test]
async fn adding_a_recipe_for_an_item_that_has_one_is_refused_rather_than_replacing_it() {
    let router = inventory_app(provisioned_admin(), FakeInventory::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let item = ulid_text(500);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(&tenant, &item, 2).0,
            &cookie,
        ))
        .await
        .expect("route create recipe");
    assert_eq!(created.status(), StatusCode::CREATED);
    let at_create = etag_of(&created);

    let again = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(&tenant, &item, 9).0,
            &cookie,
        ))
        .await
        .expect("route the duplicate create");
    assert_eq!(again.status(), StatusCode::CONFLICT);

    // The read-one carries the version as an `ETag` and as an `etag` field, and the threshold the
    // refused create did not get to overwrite.
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/inventory/recipes/{item}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route read recipe");
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(etag_of(&read), at_create);
    let fetched = json_body(read).await;
    assert_eq!(fetched["auto_86_threshold"], 2);
    assert_eq!(fetched["etag"].as_str().expect("an etag field"), at_create);

    // Deleting frees the item, so a create at it succeeds again.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/inventory/recipes/{item}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route delete recipe");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let recreated = router
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(&tenant, &item, 2).0,
            &cookie,
        ))
        .await
        .expect("route create after delete");
    assert_eq!(recreated.status(), StatusCode::CREATED);
}

/// Editing a recipe needs the version it was read at: unconditional is refused, the right one
/// applies, and a spent one is a `412`.
#[tokio::test]
async fn editing_a_recipe_needs_the_version_the_read_carried() {
    let router = inventory_app(provisioned_admin(), FakeInventory::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let item = ulid_text(500);
    let row = format!("/admin/inventory/recipes/{item}");

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/inventory/recipes",
            &recipe_bodies(&tenant, &item, 2).0,
            &cookie,
        ))
        .await
        .expect("route create recipe");
    let at_create = etag_of(&created);

    let unconditional = router
        .clone()
        .oneshot(put_with_cookie(
            &row,
            &recipe_bodies(&tenant, &item, 5).1,
            &cookie,
        ))
        .await
        .expect("route the unconditional edit");
    assert_eq!(unconditional.status(), StatusCode::BAD_REQUEST);

    let edited = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &recipe_bodies(&tenant, &item, 5).1,
            &cookie,
            &at_create,
        ))
        .await
        .expect("route edit recipe");
    assert_eq!(edited.status(), StatusCode::OK);
    let at_edit = etag_of(&edited);
    assert_ne!(at_edit, at_create, "an applied edit moves the version");

    let replayed = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &recipe_bodies(&tenant, &item, 7).1,
            &cookie,
            &at_create,
        ))
        .await
        .expect("route the stale edit");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/inventory/recipes?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list recipes");
    let rows = json_body(listed).await;
    assert_eq!(
        rows.as_array().expect("array").len(),
        1,
        "neither the refused edit nor the applied one added a row"
    );
    assert_eq!(rows[0]["auto_86_threshold"], 5);
    assert_eq!(rows[0]["etag"].as_str().expect("a row etag"), at_edit);
}
