// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The catalog's courses, modifier groups, menus, sections and placements (ADR-0066).

use axum::http::StatusCode;
use tower::ServiceExt;

use crate::{
    FakeCatalog, admin_cookie, catalog_app, delete_with_cookie, etag_of, get, get_with_cookie,
    json_body, patch_with_etag, post_with_cookie, provisioned_admin, put_with_cookie,
    put_with_etag, ulid_text,
};

/// A course can be created, listed **in service order**, renamed, moved and archived
/// ([ADR-0130](../../../../docs/adr/0130-a-course-is-something-the-catalog-names.md)).
///
/// Every `course_id` in the tree pointed at nothing before this: `sales.order_line.added` carried
/// one, a station routing rule matched on one and the edge honoured it, and there was no entity to
/// name. The listing order is asserted because the order *is* the meaning — starter before main
/// before dessert — and a course list handed back in insertion order is a taxonomy rather than a
/// service sequence.
#[tokio::test]
async fn catalog_courses_round_trip_and_list_in_service_order() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // Created out of order on purpose: the sequence has to come from `sort`, not from the order
    // somebody happened to type them in.
    let mut ids = Vec::new();
    for (name, sort) in [("Dessert", 30), ("Starter", 10), ("Main", 20)] {
        let created = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/catalog/courses",
                &serde_json::json!({ "tenant_id": tenant, "name": name, "sort": sort }),
                &cookie,
            ))
            .await
            .expect("route create course");
        assert_eq!(created.status(), StatusCode::CREATED);
        let body = json_body(created).await;
        assert_eq!(body["name"], name);
        assert_eq!(body["sort"], sort);
        ids.push((
            body["course_id"].as_str().expect("a course id").to_owned(),
            body["etag"].as_str().expect("an etag").to_owned(),
        ));
    }

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/courses?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list courses");
    let rows = json_body(listed).await;
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|row| row["name"].as_str().expect("a name"))
            .collect::<Vec<_>>(),
        vec!["Starter", "Main", "Dessert"],
        "the list is the service sequence, not the order they were typed in"
    );

    // A course moves and archives in one PATCH. A store that serves dessert before the main course
    // for a tasting menu says so this way; nothing here privileges a particular sequence.
    let (dessert_id, dessert_etag) = &ids[0];
    let updated = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/courses/{dessert_id}"),
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Pudding",
                "sort": 5,
                "status": "archived",
            }),
            &cookie,
            dessert_etag,
        ))
        .await
        .expect("route update course");
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = json_body(updated).await;
    assert_eq!(updated["name"], "Pudding");
    assert_eq!(updated["sort"], 5);
    assert_eq!(updated["status"], "archived");

    // A nameless course is refused: a course with no name is a position nobody can pick.
    let nameless = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/courses",
            &serde_json::json!({ "tenant_id": tenant, "name": "   ", "sort": 0 }),
            &cookie,
        ))
        .await
        .expect("route create nameless course");
    assert_eq!(nameless.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn catalog_modifier_groups_round_trip_with_members_and_attachments() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let modifier = ulid_text(600);
    let pizza = ulid_text(500);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/modifier-groups",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Size",
                "min_select": 1,
                "max_select": 1,
                "member_item_ids": [modifier],
                "attached_item_ids": [pizza],
            }),
            &cookie,
        ))
        .await
        .expect("route create modifier group");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Size");
    assert_eq!(created["min_select"], 1);
    assert_eq!(created["member_item_ids"][0], modifier);
    assert_eq!(created["attached_item_ids"][0], pizza);
    let etag = created["etag"].as_str().expect("an etag").to_owned();
    let group_id = created["modifier_group_id"]
        .as_str()
        .expect("a group id")
        .to_owned();

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/modifier-groups?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list modifier groups");
    assert_eq!(json_body(listed).await.as_array().expect("array").len(), 1);

    // Update the rule + archive in one PATCH.
    let updated = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/catalog/modifier-groups/{group_id}"),
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Size",
                "min_select": 0,
                "max_select": 2,
                "member_item_ids": [modifier],
                "attached_item_ids": [],
                "status": "archived",
            }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route update modifier group");
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = json_body(updated).await;
    assert_eq!(updated["max_select"], 2);
    assert_eq!(updated["status"], "archived");
    assert_eq!(
        updated["attached_item_ids"]
            .as_array()
            .expect("array")
            .len(),
        0
    );

    // A malformed member id is rejected.
    let bad = router
        .oneshot(post_with_cookie(
            "/admin/catalog/modifier-groups",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Broken",
                "member_item_ids": ["not-a-ulid"],
            }),
            &cookie,
        ))
        .await
        .expect("route create with bad member");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}

/// The `(menu, item)` pair a placement is keyed by, and the two bodies the routes take: a `POST` to
/// the collection names the pair, a `PUT` to the row takes it from the path.
fn placement_bodies(
    tenant: &str,
    menu: &str,
    item: &str,
    price: i64,
) -> (serde_json::Value, serde_json::Value) {
    let write = serde_json::json!({
        "tenant_id": tenant,
        "prices": [{ "sales_channel": "DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": price } }],
        "available": true,
    });
    let mut create = write.clone();
    create["menu_id"] = serde_json::Value::String(menu.to_owned());
    create["menu_item_id"] = serde_json::Value::String(item.to_owned());
    (create, write)
}

#[tokio::test]
async fn adding_an_item_already_on_a_menu_is_refused_rather_than_repricing_it() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let menu = ulid_text(10);
    let item = ulid_text(500);
    let base = format!("/admin/catalog/menus/{menu}/placements");

    // Adding an item to a menu is a POST to the collection, and it answers with the version the row
    // starts at.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            &base,
            &placement_bodies(&tenant, &menu, &item, 150_000).0,
            &cookie,
        ))
        .await
        .expect("route create placement");
    assert_eq!(created.status(), StatusCode::CREATED);
    let at_create = etag_of(&created);

    // A second create at the same pair is refused rather than repricing the one already there —
    // the overwrite this route used to perform silently (ADR-0095). Because the per-channel prices
    // are the price-change journal (ADR-0069), that overwrite left no `before` behind.
    let again = router
        .clone()
        .oneshot(post_with_cookie(
            &base,
            &placement_bodies(&tenant, &menu, &item, 160_000).0,
            &cookie,
        ))
        .await
        .expect("route the duplicate create");
    assert_eq!(again.status(), StatusCode::CONFLICT);

    // The list carries a version per row, which is where a reprice gets the token it must send.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{base}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list placements");
    let rows = json_body(listed).await;
    assert_eq!(
        rows.as_array().expect("array").len(),
        1,
        "a refused create adds no row"
    );
    assert_eq!(
        rows[0]["prices"][0]["unit_price"]["amount_minor"], 150_000,
        "and does not reprice the placement it refused to overwrite"
    );
    assert_eq!(
        rows[0]["etag"].as_str().expect("the row carries an etag"),
        at_create,
        "byte-identical to the token the create answered with"
    );

    // Remove it, then removing it again is a 404.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("{base}/{item}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route remove placement");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let gone = router
        .oneshot(delete_with_cookie(
            &format!("{base}/{item}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route remove missing");
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_reprice_needs_the_version_the_list_carried() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let menu = ulid_text(10);
    let item = ulid_text(500);
    let base = format!("/admin/catalog/menus/{menu}/placements");
    let row = format!("{base}/{item}");

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            &base,
            &placement_bodies(&tenant, &menu, &item, 150_000).0,
            &cookie,
        ))
        .await
        .expect("route create placement");
    assert_eq!(created.status(), StatusCode::CREATED);
    let at_create = etag_of(&created);

    // A reprice without the version is refused outright, then applies with it.
    let unconditional = router
        .clone()
        .oneshot(put_with_cookie(
            &row,
            &placement_bodies(&tenant, &menu, &item, 160_000).1,
            &cookie,
        ))
        .await
        .expect("route the unconditional reprice");
    assert_eq!(unconditional.status(), StatusCode::BAD_REQUEST);

    let repriced = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &placement_bodies(&tenant, &menu, &item, 160_000).1,
            &cookie,
            &at_create,
        ))
        .await
        .expect("route reprice placement");
    assert_eq!(repriced.status(), StatusCode::OK);

    // And replaying the version just spent is the lost update, refused.
    let replayed = router
        .clone()
        .oneshot(put_with_etag(
            &row,
            &placement_bodies(&tenant, &menu, &item, 170_000).1,
            &cookie,
            &at_create,
        ))
        .await
        .expect("route the stale reprice");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);

    let listed = router
        .oneshot(get_with_cookie(
            &format!("{base}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list placements");
    let rows = json_body(listed).await;
    assert_eq!(
        rows.as_array().expect("array").len(),
        1,
        "an update does not add a row"
    );
    assert_eq!(rows[0]["prices"][0]["unit_price"]["amount_minor"], 160_000);
    assert_ne!(
        rows[0]["etag"].as_str().expect("an etag"),
        at_create,
        "and the version moves with the write"
    );
}

#[tokio::test]
async fn catalog_menu_sections_group_placements_within_a_menu() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let menu = ulid_text(10);
    let item = ulid_text(500);
    let sections_base = format!("/admin/catalog/menus/{menu}/sections");

    // Create a section under the menu.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            &sections_base,
            &serde_json::json!({ "tenant_id": tenant, "name": "Starters", "sort": 1 }),
            &cookie,
        ))
        .await
        .expect("route create section");
    assert_eq!(created.status(), StatusCode::CREATED);
    let section = json_body(created).await;
    let section_id = section["menu_section_id"].as_str().expect("id").to_owned();
    let etag = section["etag"].as_str().expect("an etag").to_owned();
    assert_eq!(section["name"], "Starters");

    // List sections — the new one is there.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{sections_base}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list sections");
    let rows = json_body(listed).await;
    assert_eq!(rows.as_array().expect("array").len(), 1);
    assert_eq!(rows[0]["menu_section_id"], section_id);

    // Rename and re-sort it.
    let renamed = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("{sections_base}/{section_id}"),
            &serde_json::json!({
                "tenant_id": tenant, "name": "Appetizers", "sort": 2, "status": "active",
            }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename section");
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(json_body(renamed).await["name"], "Appetizers");

    // A placement can name the section it sits under, and the listing carries it back.
    let placement = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/catalog/menus/{menu}/placements"),
            &serde_json::json!({
                "menu_id": menu,
                "menu_item_id": item,
                "tenant_id": tenant,
                "menu_section_id": section_id,
                "prices": [{ "sales_channel": "DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": 150_000 } }],
                "available": true,
            }),
            &cookie,
        ))
        .await
        .expect("route create placement in section");
    assert_eq!(placement.status(), StatusCode::CREATED);

    let placements = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/menus/{menu}/placements?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list placements");
    let rows = json_body(placements).await;
    assert_eq!(rows[0]["menu_section_id"], section_id);
}

#[tokio::test]
async fn catalog_is_behind_the_session_guard() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    // A well-formed request (valid `tenant_id`) but no session cookie → the guard denies before any
    // listing is revealed. The query is well-formed so the guard, not the extractor, is what refuses.
    let denied = router
        .oneshot(get(
            &format!("/admin/catalog/items?tenant_id={}", ulid_text(1)),
            None,
        ))
        .await
        .expect("route unauthenticated");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}
