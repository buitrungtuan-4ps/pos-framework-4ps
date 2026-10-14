// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The menu publish (`POST /admin/catalog/publish`, ADR-0066): the compiled book and layout on a store.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::fees::InMemoryFeeRules;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeCatalog, FakeConfigTrees, FakeKeys, FakeRegistry, FakeRollups, FakeTaxRates,
    FakeWebhooks, admin_cookie, app_all, author_a_menu, clock, get_with_cookie, json_body,
    post_with_cookie, provisioned_admin, put_with_cookie, ulid_text,
};

/// The main router (for login + the effective-config read), the catalog CRUD router, and the publish
/// router — all sharing one admin, one catalog, and one config-tree store, as production merges them.
///
/// The tax and locale publish routers ride along because a menu publish now requires both nodes
/// ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §7, finding **F7**):
/// without them a test would have to seed the tree behind the routes to reach the path it is
/// testing, which is exactly the shortcut that let the rule be built and never exercised end to end.
fn catalog_publish_app(
    admin: FakeAdmin,
    catalog: FakeCatalog,
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
    http::router(app)
        .merge(http::catalog_router(
            catalog.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::config_tax_router(
            FakeTaxRates::default(),
            config_trees.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::config_locale_router(
            &pos_cloud::countries::registry(),
            config_trees.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::catalog_publish_router(
            catalog,
            InMemoryFeeRules::new(),
            FakeRegistry::default(),
            config_trees,
            admin,
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
}

/// Publishes the `tax` and `locale` nodes a menu publish requires, in the order an operator has to.
///
/// Both go through the real routes rather than being written into the tree, because the sequence
/// *is* the thing ADR-0122 §7 imposes on whoever sets a shop up: tax table, then locale, then the
/// menu. A tax table with no rows still counts — the rule is about a store having been told what
/// its taxes are, and "none" is an answer a tenant may legitimately give.
async fn publish_menu_prerequisites(
    router: &axum::Router,
    cookie: &str,
    tenant: &str,
    store: &str,
) {
    let tax = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/tax",
            &serde_json::json!({ "tenant_id": tenant, "store_id": store }),
            cookie,
        ))
        .await
        .expect("route tax publish");
    assert_eq!(tax.status(), StatusCode::OK);

    let locale = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/locale",
            &serde_json::json!({
                "tenant_id": tenant,
                "store_id": store,
                "country_code": "VN",
                "currency_code": "VND",
                "timezone": "Asia/Ho_Chi_Minh",
                "cutoff_hour": 4,
            }),
            cookie,
        ))
        .await
        .expect("route locale publish");
    assert_eq!(locale.status(), StatusCode::OK);
}

/// Authors a course and returns its id (ADR-0130).
async fn create_course(
    router: &axum::Router,
    cookie: &str,
    tenant: &str,
    name: &str,
    sort: i32,
) -> String {
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/courses",
            &serde_json::json!({ "tenant_id": tenant, "name": name, "sort": sort }),
            cookie,
        ))
        .await
        .expect("route create course");
    assert_eq!(created.status(), StatusCode::CREATED);
    json_body(created).await["course_id"]
        .as_str()
        .expect("a course id")
        .to_owned()
}

/// Authors an item on `course_id`, places it on `menu_id` at a dine-in price, and returns its id.
async fn place_item_on_course(
    router: &axum::Router,
    cookie: &str,
    tenant: &str,
    menu_id: &str,
    (name, course_id, minor): (&str, &str, i64),
) -> String {
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({
                "tenant_id": tenant,
                "name": name,
                "tax_class_id": ulid_text(7),
                "course_id": course_id,
            }),
            cookie,
        ))
        .await
        .expect("route create item");
    assert_eq!(created.status(), StatusCode::CREATED);
    let item_id = json_body(created).await["menu_item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();

    let placed = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/catalog/menus/{menu_id}/placements"),
            &serde_json::json!({
                "menu_id": menu_id,
                "menu_item_id": item_id,
                "tenant_id": tenant,
                "prices": [{ "sales_channel": "SALES_CHANNEL_DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": minor } }],
                "available": true,
            }),
            cookie,
        ))
        .await
        .expect("route place item");
    assert_eq!(placed.status(), StatusCode::CREATED);
    item_id
}

#[tokio::test]
async fn publishing_a_menu_writes_the_compiled_book_onto_the_store_config() {
    let router = catalog_publish_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeConfigTrees::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let store = ulid_text(2);

    // Author an item, a menu, and a dine-in placement.
    let item = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/items",
            &serde_json::json!({ "tenant_id": tenant, "name": "Margherita", "tax_class_id": ulid_text(7) }),
            &cookie,
        ))
        .await
        .expect("route create item");
    let item_id = json_body(item).await["menu_item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();

    let menu = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create menu");
    let menu_id = json_body(menu).await["menu_id"]
        .as_str()
        .expect("a menu id")
        .to_owned();

    let placed = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/catalog/menus/{menu_id}/placements"),
            &serde_json::json!({
                "menu_id": menu_id,
                "menu_item_id": item_id,
                "tenant_id": tenant,
                "prices": [{ "sales_channel": "SALES_CHANNEL_DINE_IN", "unit_price": { "currency_code": "VND", "amount_minor": 150_000 } }],
                "available": true,
            }),
            &cookie,
        ))
        .await
        .expect("route place item");
    assert_eq!(placed.status(), StatusCode::CREATED);

    // What the shop must already hold before a menu may be published to it (ADR-0122 §7).
    publish_menu_prerequisites(&router, &cookie, &tenant, &store).await;

    // Publish the menu to the store.
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({ "tenant_id": tenant, "store_id": store, "menu_id": menu_id }),
            &cookie,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);

    // The store's effective config now carries the compiled MenuBook on its `menu` node.
    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store}/config?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route effective config");
    assert_eq!(effective.status(), StatusCode::OK);
    let doc = json_body(effective).await;
    let dine_in = &doc["menu"]["channels"][0];
    assert_eq!(dine_in["sales_channel"], "SALES_CHANNEL_DINE_IN");
    let entry = &dine_in["catalog"]["items"][0];
    assert_eq!(entry["menu_item_id"], item_id);
    assert_eq!(entry["unit_price"]["amount_minor"], 150_000);
    assert_eq!(entry["display_name"], "Margherita");
}

/// The half [ADR-0130](../../../docs/adr/0130-a-course-is-something-the-catalog-names.md) named as
/// missing when the course entity landed: an operator could author a course and put an item on it,
/// and the compiled `menu` node carried neither, so a store received a price book with no courses in
/// it and every `course_id` on the wire still pointed at nothing a till could name.
///
/// End to end through the admin API on purpose. The compiler's own tests prove the rules; this
/// proves the wiring between them — `list_courses` is actually called on the publish path, and what
/// comes out reaches the store's effective config.
#[tokio::test]
async fn a_published_menu_carries_the_courses_its_items_go_out_on() {
    let router = catalog_publish_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeConfigTrees::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let store = ulid_text(2);

    // Authored dessert-first, so the published order cannot have come from insertion order.
    let dessert = create_course(&router, &cookie, &tenant, "Dessert", 30).await;
    let main = create_course(&router, &cookie, &tenant, "Main", 20).await;

    let menu = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create menu");
    let menu_id = json_body(menu).await["menu_id"]
        .as_str()
        .expect("a menu id")
        .to_owned();

    // A pizza on the main course and a tiramisu on dessert, both priced dine-in.
    let pizza_id = place_item_on_course(
        &router,
        &cookie,
        &tenant,
        &menu_id,
        ("Margherita", &main, 150_000),
    )
    .await;
    place_item_on_course(
        &router,
        &cookie,
        &tenant,
        &menu_id,
        ("Tiramisu", &dessert, 80_000),
    )
    .await;

    publish_menu_prerequisites(&router, &cookie, &tenant, &store).await;

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({ "tenant_id": tenant, "store_id": store, "menu_id": menu_id }),
            &cookie,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);

    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store}/config?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route effective config");
    assert_eq!(effective.status(), StatusCode::OK);
    let doc = json_body(effective).await;
    let catalog = &doc["menu"]["channels"][0]["catalog"];

    let courses = catalog["courses"].as_array().expect("the courses");
    assert_eq!(courses.len(), 2, "the store receives both courses");
    assert_eq!(
        courses
            .iter()
            .map(|course| course["display_name"].as_str().expect("a name"))
            .collect::<Vec<_>>(),
        ["Main", "Dessert"],
        "in service order, not the order they were authored"
    );

    let pizza = catalog["items"]
        .as_array()
        .expect("the items")
        .iter()
        .find(|entry| entry["menu_item_id"] == pizza_id.as_str())
        .expect("the pizza");
    assert_eq!(
        pizza["course_id"], main,
        "and each entry names the course it goes out on"
    );
}

#[tokio::test]
async fn publishing_an_unknown_menu_is_refused() {
    let router = catalog_publish_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeConfigTrees::default(),
    );
    let cookie = admin_cookie(&router).await;
    // No menu authored → the compiler refuses with a named error, surfaced as 422 (not a store 5xx).
    let refused = router
        .oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({ "tenant_id": ulid_text(1), "store_id": ulid_text(2), "menu_id": ulid_text(10) }),
            &cookie,
        ))
        .await
        .expect("route publish unknown menu");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

/// Finding **F7**: the single-store path published a menu to a shop with no tax table.
///
/// The batch path had refused exactly this since store groups landed, so the same instruction got
/// two answers depending on which button an operator pressed — and the permissive one produced a
/// shop that boots, syncs, shows the menu, takes the order, and raises `TaxRateNotConfigured` at
/// the payment screen. Both halves are asserted here: the refusal names what is missing, and the
/// same publish succeeds once the operator has done what the refusal told them to.
#[tokio::test]
async fn publishing_a_menu_to_a_store_with_no_tax_table_is_refused_and_says_what_is_missing() {
    let router = catalog_publish_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeConfigTrees::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let store = ulid_text(2);
    let menu_id = author_a_menu(&router, &cookie, &tenant).await;

    let publish = || {
        router.clone().oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({ "tenant_id": tenant, "store_id": store, "menu_id": menu_id }),
            &cookie,
        ))
    };

    let refused = publish().await.expect("route the premature publish");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let message = json_body(refused).await["error"]["message"]
        .as_str()
        .expect("a message")
        .to_owned();
    assert!(
        message.contains("`tax`"),
        "the refusal names the node to publish first, which is the difference between an operator \
         fixing it and an operator reading the ADR: {message}"
    );

    // Nothing was written: a refused publish leaves the shop exactly as it was.
    let untouched = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store}/config?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the effective config");
    assert!(
        json_body(untouched).await["menu"].is_null(),
        "the store has no menu node, so the refusal cost it nothing"
    );

    publish_menu_prerequisites(&router, &cookie, &tenant, &store).await;
    let accepted = publish()
        .await
        .expect("route the publish that follows the rule");
    assert_eq!(
        accepted.status(),
        StatusCode::OK,
        "doing what the refusal asked for is enough; there is no second hurdle"
    );
}

#[tokio::test]
async fn publishing_also_writes_the_compiled_layout_onto_the_store_config() {
    let router = catalog_publish_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeConfigTrees::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let store = ulid_text(2);

    // A menu to publish (empty is fine — the layout is compiled independently).
    let menu = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/menus",
            &serde_json::json!({ "tenant_id": tenant, "name": "Standard" }),
            &cookie,
        ))
        .await
        .expect("route create menu");
    let menu_id = json_body(menu).await["menu_id"]
        .as_str()
        .expect("a menu id")
        .to_owned();

    // A display category and a dine-in layout button under it.
    let category = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/display-categories",
            &serde_json::json!({ "tenant_id": tenant, "name": "Pizza" }),
            &cookie,
        ))
        .await
        .expect("route create display category");
    let category_id = json_body(category).await["display_category_id"]
        .as_str()
        .expect("a display category id")
        .to_owned();

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

    // The same two nodes the menu publish requires — the layout rides along on that publish, so it
    // inherits its prerequisites (ADR-0122 §7).
    publish_menu_prerequisites(&router, &cookie, &tenant, &store).await;

    // Publish the menu; the layout rides along on the same publish.
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({ "tenant_id": tenant, "store_id": store, "menu_id": menu_id }),
            &cookie,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);

    // The store's effective config now carries a compiled DisplayPlan on its `layout` node.
    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store}/config?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route effective config");
    let doc = json_body(effective).await;
    let channel = &doc["layout"]["channels"][0];
    assert_eq!(channel["sales_channel"], "SALES_CHANNEL_DINE_IN");
    let category = &channel["plan"]["categories"][0];
    assert_eq!(category["name"], "Pizza");
    assert_eq!(category["buttons"][0]["label"], "Margherita");
    assert_eq!(category["buttons"][0]["menu_item_id"], item);
}
