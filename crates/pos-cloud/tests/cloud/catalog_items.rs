// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The catalog item routes (ADR-0066): the item list, paging and search, and the item import.

use axum::http::StatusCode;
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeCatalog, admin_cookie, catalog_app, get_with_cookie, json_body, post_bytes_with_cookie,
    post_with_cookie, provisioned_admin, text_body, ulid_text,
};

/// The item-master CSV import, end to end over the routes (roadmap-v3 **F15**).
///
/// The dry run must write nothing and the apply must write what it promised — the two properties an
/// operator is trusting when they press the button on a file of hundreds of rows.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one file through both halves of the rail, asserted end to end — the point is that the \
              dry run and the apply agree about the same bytes, so splitting them into two tests \
              would duplicate the whole fixture to check half the property each time"
)]
async fn an_item_import_reviews_without_writing_and_then_applies_what_it_promised() {
    let catalog = FakeCatalog::default();
    let router = catalog_app(provisioned_admin(), catalog.clone());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // A tax class to reference, and one existing item to update.
    let tax_class = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/tax-classes",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create tax class");
    assert_eq!(tax_class.status(), StatusCode::CREATED);
    let tax_class_id = json_body(tax_class).await["tax_class_id"]
        .as_str()
        .expect("a tax class id")
        .to_owned();

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Pho",
                "tax_class_id": tax_class_id,
                "name_translations": { "vi": "Phở" },
            }),
            &cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(created.status(), StatusCode::CREATED);
    let menu_item_id = json_body(created).await["menu_item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();

    let csv = format!(
        "menu_item_id,name,status,tax_class_id,item_category_id,item_subcategory_id,image_ref\n\
         {menu_item_id},Pho bo,active,{tax_class_id},,,\n\
         ,Tra da,active,{tax_class_id},,,\n\
         ,No tax,active,not-an-id,,,\n"
    );

    let reviewed = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/catalog/import/items/dry-run?tenant_id={tenant}"),
            csv.clone().into_bytes(),
            "text/csv",
            &cookie,
        ))
        .await
        .expect("route the dry run");
    assert_eq!(reviewed.status(), StatusCode::OK);
    let report = json_body(reviewed).await;
    assert_eq!(report["update_count"], 1);
    assert_eq!(report["create_count"], 1);
    assert_eq!(report["reject_count"], 1);
    assert_eq!(
        catalog
            .tenant_items(TenantId::new(Ulid::from_u128(1)))
            .len(),
        1,
        "a dry run that wrote a row would be a dry run nobody can trust"
    );

    let applied = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/catalog/import/items/apply?tenant_id={tenant}"),
            csv.into_bytes(),
            "text/csv",
            &cookie,
        ))
        .await
        .expect("route the apply");
    assert_eq!(applied.status(), StatusCode::OK);
    let report = json_body(applied).await;
    assert_eq!(report["create_count"], 1);
    assert_eq!(report["update_count"], 1);
    assert_eq!(report["reject_count"], 1, "the bad row is still refused");

    let items = catalog.tenant_items(TenantId::new(Ulid::from_u128(1)));
    assert_eq!(items.len(), 2, "one created, one updated in place");
    let updated = items
        .iter()
        .find(|row| row.record.menu_item_id.to_string() == menu_item_id)
        .expect("the updated item");
    assert_eq!(updated.record.name, "Pho bo");
    assert_eq!(
        updated
            .record
            .name_translations
            .get("vi")
            .map(String::as_str),
        Some("Phở"),
        "the import kept the per-locale name the CSV format cannot carry"
    );
    assert!(
        items.iter().any(|row| row.record.name == "Tra da"),
        "the row naming no id was created"
    );
    assert!(
        !items.iter().any(|row| row.record.name == "No tax"),
        "the rejected row was never written"
    );
}

#[tokio::test]
async fn catalog_creates_and_lists_an_item_and_a_menu() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Margherita",
                "tax_class_id": ulid_text(7),
                // Per-locale names (ADR-0074): a real one, plus a blank row the handler drops.
                "name_translations": { "vi": "Bánh Margherita", "": "ignored", "ja": "  " },
                // An item photo (ADR-0075) — a media id round-tripped as image_ref.
                "image_ref": ulid_text(42),
            }),
            &cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Margherita");
    assert_eq!(created["status"], "active");
    assert_eq!(
        created["image_ref"],
        ulid_text(42),
        "the item photo round-trips"
    );
    assert_eq!(
        created["name_translations"]["vi"], "Bánh Margherita",
        "a real per-locale name is kept"
    );
    assert!(
        created["name_translations"].get("").is_none()
            && created["name_translations"].get("ja").is_none(),
        "a blank-key or blank-value translation row is dropped"
    );
    let item_id = created["menu_item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/items?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list items");
    assert_eq!(listed.status(), StatusCode::OK);
    let items = json_body(listed).await;
    assert_eq!(items.as_array().expect("array").len(), 1);
    assert_eq!(items[0]["menu_item_id"], item_id);
    assert_eq!(items[0]["name_translations"]["vi"], "Bánh Margherita");

    // The CSV export (ADR-0075, Track M5): the item master as text/csv, no price column.
    let exported = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/export/items?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route export items");
    assert_eq!(exported.status(), StatusCode::OK);
    let csv = text_body(exported).await;
    let mut lines = csv.lines();
    assert_eq!(
        lines.next().unwrap(),
        "menu_item_id,name,status,tax_class_id,item_category_id,item_subcategory_id,image_ref"
    );
    let row = lines.next().expect("one item row");
    assert!(row.contains("Margherita") && row.contains(&item_id));
    assert!(row.contains(&ulid_text(42)), "the image ref is a column");
    assert!(
        !csv.to_lowercase().contains("price"),
        "no price is exported"
    );

    // A menu, optionally with a parent — created by name, id minted server-side.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create menu");
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        json_body(created).await["parent_menu_id"],
        serde_json::Value::Null
    );

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/menus?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list menus");
    assert_eq!(json_body(listed).await.as_array().expect("array").len(), 1);
}

#[tokio::test]
async fn a_paged_item_read_searches_and_sorts_on_the_server() {
    // The wire half of B3-3: `?q=`, `?sort=` and `?order=` have to reach the store, not be parsed
    // and dropped. Each assertion here would pass against a handler that ignored its parameter if
    // the fixture were smaller — so the fixture is three items whose name order differs from their
    // creation order.
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    for name in ["Zucchini", "Anchovy", "Margherita"] {
        let created = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/catalog/items",
                &serde_json::json!({
                    "tenant_id": tenant,
                    "name": name,
                    "tax_class_id": ulid_text(7),
                    "name_translations": { "vi": format!("Bánh {name}") },
                }),
                &cookie,
            ))
            .await
            .expect("route create item");
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    let names = |body: &serde_json::Value| -> Vec<String> {
        body["items"]
            .as_array()
            .expect("the window")
            .iter()
            .map(|row| row["name"].as_str().expect("a name").to_owned())
            .collect()
    };
    let read = |query: &str| {
        let router = router.clone();
        let cookie = cookie.clone();
        let uri = format!("/admin/catalog/items?tenant_id={tenant}&{query}");
        async move {
            let response = router
                .oneshot(get_with_cookie(&uri, &cookie))
                .await
                .expect("route the paged read");
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        }
    };

    // Default order is newest-first, unchanged from the unpaged read.
    let newest = read("limit=10").await;
    assert_eq!(names(&newest), ["Margherita", "Anchovy", "Zucchini"]);

    // `?sort=name` is a different order, and `?order=desc` reverses it.
    let by_name = read("limit=10&sort=name").await;
    assert_eq!(names(&by_name), ["Anchovy", "Margherita", "Zucchini"]);
    let reversed = read("limit=10&sort=name&order=desc").await;
    assert_eq!(names(&reversed), ["Zucchini", "Margherita", "Anchovy"]);

    // `?q=` narrows the rows *and* the total, so the pager does not offer empty pages.
    let searched = read("limit=10&q=anch").await;
    assert_eq!(names(&searched), ["Anchovy"]);
    assert_eq!(searched["total"], 1, "the total counts the match");

    // The search reaches the per-locale names too (ADR-0074): "Bánh" is in no primary name.
    let vietnamese = read("limit=10&q=B%C3%A1nh").await;
    assert_eq!(
        vietnamese["total"], 3,
        "every item's Vietnamese name matches"
    );

    // A search and a sort compose, and the page still carries the matching total.
    //
    // The needle is `i`, chosen after this test first asserted the wrong number: every per-locale
    // name here begins "Bánh", so a needle containing `n`, `a` or `h` matches all three through the
    // translation and proves nothing about narrowing. `i` appears in Zucchini and Margherita, in
    // neither Anchovy nor "Bánh".
    let both = read("limit=1&sort=name&q=i").await;
    assert_eq!(
        names(&both),
        ["Margherita"],
        "first by name among the matches, not the first match in the default order",
    );
    assert_eq!(both["total"], 2, "Margherita and Zucchini contain an i");
}

#[tokio::test]
async fn the_item_list_returns_the_whole_master_unpaged_and_a_page_when_a_limit_is_named() {
    let router = catalog_app(provisioned_admin(), FakeCatalog::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": "Margherita",
                "tax_class_id": ulid_text(7),
            }),
            &cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(created.status(), StatusCode::CREATED);
    let item_id = json_body(created).await["menu_item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();

    // An absent limit is the whole item master as a bare array, which the menu compiler and the
    // item pickers depend on — five of the six console consumers of this read are not tables.
    let whole = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/items?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the whole-set list");
    assert_eq!(
        json_body(whole)
            .await
            .as_array()
            .expect("a bare array, not an envelope")
            .len(),
        1,
    );

    // Naming a limit asks for a page instead (ADR-0098): the window, plus the size of the master.
    let paged = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/items?tenant_id={tenant}&limit=1"),
            &cookie,
        ))
        .await
        .expect("route the paged list");
    assert_eq!(paged.status(), StatusCode::OK);
    let paged = json_body(paged).await;
    assert_eq!(paged["items"].as_array().expect("the window").len(), 1);
    assert_eq!(paged["items"][0]["menu_item_id"], item_id);
    assert_eq!(paged["total"], 1, "one item in the master");
    assert_eq!(paged["limit"], 1, "the bounds used are echoed back");
    assert_eq!(paged["offset"], 0);
    assert!(
        paged["items"][0]["etag"].is_string(),
        "a paged row keeps the per-row etag ADR-0095 put on it",
    );

    // A page past the end is empty and not an error, and still reports the master's size.
    let beyond = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/items?tenant_id={tenant}&limit=10&offset=50"),
            &cookie,
        ))
        .await
        .expect("route the page past the end");
    let beyond = json_body(beyond).await;
    assert!(beyond["items"].as_array().expect("the window").is_empty());
    assert_eq!(beyond["total"], 1);

    for (bound, field) in [
        ("limit=100000", "limit"),
        ("limit=0", "limit"),
        ("offset=1", "offset"),
        // A page-shaping parameter without a limit: the fix is "add a limit", not "correct the
        // value", so it gets its own sentence and names the parameter it arrived with.
        ("q=marg", "q"),
        ("sort=name", "sort"),
        ("order=desc", "order"),
        // Inside the paged form, a token outside the route's closed set is refused with the list of
        // what it accepts, rather than answered with the default order.
        ("limit=5&sort=price", "sort"),
        ("limit=5&order=sideways", "order"),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/catalog/items?tenant_id={tenant}&{bound}"),
                &cookie,
            ))
            .await
            .expect("route the bad bound");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "`{bound}` is a client mistake, not a page",
        );
        assert_eq!(
            json_body(refused).await["error"]["details"][0]["field"],
            field,
            "`{bound}` names the parameter that was wrong",
        );
    }
}
