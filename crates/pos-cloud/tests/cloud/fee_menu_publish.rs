// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A menu publish brings the store's fees along (ADR-0159).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::catalog::{CatalogItem, ChannelPrice, Menu, MenuId, MenuPlacement};
use pos_cloud::config_tree::{ConfigTreeState, ConfigTreeStore};
use pos_cloud::fees::{
    FeeRule, FeeRuleStore, FeeScope, InMemoryFeeRules, SampleLine, StoreFacts, sample_bill,
};
use pos_cloud::registry::EntityStatus;
use pos_cloud::store_groups::{StoreGroup, StoreGroupId};
use pos_cloud::version::{Version, Versioned};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{FeeId, StoreId};
use pos_proto::locale::TaxRate;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use tower::ServiceExt;

use super::fees::{
    fee_body, fee_catalog, fee_category, fee_item, fee_tax_class, fees_brand, fees_registry,
    fees_stores, seed_fee_stores, store_fees, tenant_scope_id,
};
use crate::{
    FakeCampaigns, FakeCatalog, FakeConfigTrees, FakeFloor, FakeInventory, FakeKeys, FakePeople,
    FakeReasonCodes, FakeRollups, FakeStoreGroups, FakeTaxRates, FakeWebhooks, NOW_MS,
    admin_cookie, app_all, clock, json_body, outcomes, post_with_cookie, provisioned_admin,
    store_group_app_with_fees, tenant,
};

// --- A menu publish brings the store's fees along (ADR-0159) ------------------------------------

/// Places item `n` on `menu_id` at 95,000 đồng on dine-in.
fn place_fee_item(catalog: &FakeCatalog, menu_id: MenuId, n: u128) {
    catalog
        .placements
        .lock()
        .expect("lock")
        .push(Versioned::new(
            MenuPlacement {
                tenant_id: tenant(),
                menu_id,
                menu_item_id: fee_item(n),
                menu_section_id: None,
                prices: vec![ChannelPrice {
                    sales_channel: Open::from_known(SalesChannel::DineIn),
                    unit_price: pos_proto::money::Money::new(
                        pos_proto::money::CurrencyCode::VND,
                        95_000,
                    ),
                }],
                available: true,
            },
            Version::new("1"),
        ));
}

/// The menu the fee tests publish: items 1 and 3, on dine-in.
fn seed_fee_menu(catalog: &FakeCatalog) -> MenuId {
    let menu_id = MenuId::new(Ulid::from_u128(0x7F01));
    catalog.menus.lock().expect("lock").push(Versioned::new(
        Menu {
            menu_id,
            tenant_id: tenant(),
            name: "Standard".to_owned(),
            parent_menu_id: None,
            status: EntityStatus::Active,
        },
        Version::new("1"),
    ));
    for n in [1, 3] {
        place_fee_item(catalog, menu_id, n);
    }
    menu_id
}

/// A new pizza: item `n`, put in the fee category and on the menu, as an operator adds one before
/// publishing the menu.
fn add_pizza(catalog: &FakeCatalog, menu_id: MenuId, n: u128) {
    catalog.items.lock().expect("lock").push(Versioned::new(
        CatalogItem {
            menu_item_id: fee_item(n),
            tenant_id: tenant(),
            name: format!("Item {n}"),
            name_translations: BTreeMap::new(),
            tax_class_id: fee_tax_class(1),
            item_category_id: Some(fee_category(1)),
            item_subcategory_id: None,
            course_id: None,
            image_ref: None,
            status: EntityStatus::Active,
        },
        Version::new("1"),
    ));
    place_fee_item(catalog, menu_id, n);
}

/// A packaging fee on every pizza, 3,000 đồng a unit, taxed at the pizzas' own class: a rule that
/// names the category, not its items.
fn pizza_packaging() -> serde_json::Value {
    serde_json::json!({
        "code": "PACKAGING",
        "display_name": "Packaging",
        "kind": "FEE_KIND_AMOUNT_PER_UNIT",
        "amount": { "currency_code": "VND", "amount_minor": 3_000 },
        "item_scope": "FEE_ITEMS_INCLUDE",
        "item_category_ids": [fee_category(1).to_string()],
        "tax": "FEE_TAX_TAX_CLASS",
        "tax_class_id": fee_tax_class(1).to_string(),
    })
}

/// A tax table that rates the second class only, so a fee taxed at the first fails every bill.
fn second_class_only() -> serde_json::Value {
    let table = pos_proto::locale::TaxRateTable::new()
        .with(
            fee_tax_class(2),
            SalesChannel::DineIn,
            TaxRate::from_basis_points(800),
        )
        .with(
            fee_tax_class(2),
            SalesChannel::Takeaway,
            TaxRate::from_basis_points(800),
        );
    serde_json::to_value(&table).expect("encode the tax table")
}

/// The menu publish, the fee routes and the generic preview over one catalog, rule store, registry
/// and config-tree fake, as `main.rs` merges them, with the fee tests' stores seeded and a menu of
/// items 1 and 3 to publish.
fn menu_fees_app() -> (axum::Router, FakeConfigTrees, FakeCatalog, MenuId) {
    let admin = provisioned_admin();
    let config_trees = FakeConfigTrees::default();
    let registry = fees_registry();
    let catalog = fee_catalog();
    let rules = InMemoryFeeRules::new();
    seed_fee_stores(&config_trees);
    let menu_id = seed_fee_menu(&catalog);
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
            rules.clone(),
            registry.clone(),
            catalog.clone(),
            config_trees.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::config_preview_router(
            config_trees.clone(),
            http::batch_nodes(
                catalog.clone(),
                FakeTaxRates::default(),
                FakeCampaigns,
                FakeInventory::default(),
                FakeReasonCodes::default(),
                FakePeople::default(),
                FakeFloor::default(),
                rules.clone(),
                registry.clone(),
            ),
            admin.clone(),
            clock(),
        ))
        .merge(http::catalog_publish_router(
            catalog.clone(),
            rules,
            registry,
            config_trees.clone(),
            admin,
            clock(),
            Arc::new(NoopAuditRecorder),
        ));
    (router, config_trees, catalog, menu_id)
}

/// Writes [`pizza_packaging`] for the brand's first store, and answers its fee id.
async fn write_pizza_packaging(router: &axum::Router, cookie: &str) -> String {
    let [first, ..] = fees_stores();
    let written = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees",
            &fee_body("FEE_SCOPE_STORE", &first.to_string(), &pizza_packaging()),
            cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::CREATED);
    json_body(written).await["fee_id"]
        .as_str()
        .expect("a fee id")
        .to_owned()
}

/// Publishes `menu_id` to `store`, and answers the body.
async fn publish_fee_menu(
    router: &axum::Router,
    cookie: &str,
    store: StoreId,
    menu_id: MenuId,
) -> serde_json::Value {
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/catalog/publish",
            &serde_json::json!({
                "tenant_id": tenant_scope_id(),
                "store_id": store.to_string(),
                "menu_id": menu_id.as_ulid().to_string(),
            }),
            cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    json_body(published).await
}

/// What publishing `menu_id` to `store` would change, as the generic preview says it.
async fn preview_fee_menu(
    router: &axum::Router,
    cookie: &str,
    store: StoreId,
    menu_id: MenuId,
) -> serde_json::Value {
    let previewed = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/preview",
            &serde_json::json!({
                "tenant_id": tenant_scope_id(),
                "store_id": store.to_string(),
                "node": "menu",
                "arguments": { "menu_id": menu_id.as_ulid().to_string() },
            }),
            cookie,
        ))
        .await
        .expect("route the preview");
    assert_eq!(previewed.status(), StatusCode::OK);
    json_body(previewed).await
}

/// A store's tree as the fake holds it.
async fn fee_store_tree(config_trees: &FakeConfigTrees, store: StoreId) -> ConfigTreeState {
    config_trees
        .load(tenant(), store)
        .await
        .expect("load")
        .expect("a tree")
        .record
}

/// The nodes the store's latest version wrote.
fn last_written(tree: &ConfigTreeState) -> Vec<String> {
    tree.history.last().expect("a version").nodes.clone()
}

/// A rule naming a category is compiled into the items it holds, so the publish that adds a pizza
/// to the category, the menu's, is the one that brings the store's fees along: in the menu's own
/// version, with no fee publish in between, and the store charges the fee on the new pizza from
/// that version on. The preview says so beforehand, because it composes as the publish does.
#[tokio::test]
async fn a_menu_publish_charges_a_category_fee_on_the_items_it_adds_in_the_same_version() {
    let (router, config_trees, catalog, menu_id) = menu_fees_app();
    let cookie = admin_cookie(&router).await;
    let [first, ..] = fees_stores();
    write_pizza_packaging(&router, &cookie).await;
    add_pizza(&catalog, menu_id, 4);

    let previewed = preview_fee_menu(&router, &cookie, first, menu_id).await;
    assert!(previewed["diff"]["fees"].is_object(), "{previewed}");

    let body = publish_fee_menu(&router, &cookie, first, menu_id).await;
    let version = body["config_version_id"].as_str().expect("a version");
    assert_eq!(body["fees"]["outcome"], "FEE_PUBLISH_APPLIED", "{body}");
    assert_eq!(
        body["fees"]["config_version_id"], version,
        "the menu's own version"
    );
    let fees = store_fees(&config_trees, first).await.expect("a fees node");
    let [fee] = fees.fees.as_slice() else {
        panic!("one fee: {fees:?}");
    };
    assert_eq!(
        fee.menu_item_ids,
        vec![fee_item(1), fee_item(2), fee_item(4)]
    );
    let tree = fee_store_tree(&config_trees, first).await;
    assert_eq!(last_written(&tree), ["menu", "layout", "fees"]);
    assert_eq!(
        tree.history.last().map(|written| written.id.to_string()),
        Some(version.to_owned())
    );

    // The store bills two of the new pizza with two units of packaging.
    let bill = sample_bill(
        &StoreFacts::from_document(&tree.effective()),
        SalesChannel::DineIn,
        &[SampleLine {
            menu_item_id: fee_item(4),
            quantity: pos_proto::quantity::Quantity::from_whole(2).expect("a quantity"),
        }],
        &fees,
    )
    .expect("a bill");
    assert_eq!(
        bill.totals.service_charge,
        pos_proto::money::Money::new(pos_proto::money::CurrencyCode::VND, 6_000)
    );
    let unchanged = preview_fee_menu(&router, &cookie, first, menu_id).await;
    assert_eq!(unchanged["unchanged"], true, "{unchanged}");
}

/// A menu publish to a store whose fees are what its rules resolve to writes no `fees` key: the
/// version names the menu and its layout only, and the answer says the fees are unchanged.
#[tokio::test]
async fn a_menu_publish_writes_no_fees_where_they_are_unchanged() {
    let (router, config_trees, _, menu_id) = menu_fees_app();
    let cookie = admin_cookie(&router).await;
    let [first, ..] = fees_stores();
    write_pizza_packaging(&router, &cookie).await;
    let held = fee_store_tree(&config_trees, first).await.layers[2]["fees"].clone();

    let body = publish_fee_menu(&router, &cookie, first, menu_id).await;
    assert_eq!(
        body["fees"],
        serde_json::json!({ "store_id": first.to_string(), "outcome": "FEE_PUBLISH_UNCHANGED" })
    );
    let tree = fee_store_tree(&config_trees, first).await;
    assert_eq!(last_written(&tree), ["menu", "layout"]);
    assert_eq!(tree.layers[2]["fees"], held);
}

/// A store that cannot apply one of its rules still gets its menu, and the answer says which rule
/// and why, in the fee publish's own tokens; the store keeps the fees it has. The fees are
/// composed on the tree the menu is written over: a tax table that no longer rates the pizzas'
/// class, landing between the publish's read and its write, is the one the rule is checked
/// against.
#[tokio::test]
async fn a_menu_publish_still_publishes_the_menu_where_the_store_cannot_apply_a_fee() {
    let (router, config_trees, catalog, menu_id) = menu_fees_app();
    let cookie = admin_cookie(&router).await;
    let [first, ..] = fees_stores();
    let fee_id = write_pizza_packaging(&router, &cookie).await;
    let held = fee_store_tree(&config_trees, first).await.layers[2]["fees"].clone();
    add_pizza(&catalog, menu_id, 4);
    *config_trees.interpose.lock().expect("lock") = Some(("tax".to_owned(), second_class_only()));

    let body = publish_fee_menu(&router, &cookie, first, menu_id).await;
    assert_eq!(body["fees"]["outcome"], "FEE_PUBLISH_REFUSED", "{body}");
    assert_eq!(
        body["fees"]["faults"],
        serde_json::json!([{ "fee_id": fee_id, "reason": "TAX_RATE_NOT_CONFIGURED" }])
    );
    let tree = fee_store_tree(&config_trees, first).await;
    assert_eq!(last_written(&tree), ["menu", "layout"]);
    assert_eq!(
        tree.layers[2]["tax"],
        second_class_only(),
        "written over, not under"
    );
    assert_eq!(
        tree.layers[2]["fees"], held,
        "the store keeps the fees it had"
    );
    let menu: pos_proto::MenuBook =
        serde_json::from_str(&tree.layers[2]["menu"].to_string()).expect("the edge reads it");
    assert!(
        menu.catalog_for(SalesChannel::DineIn)
            .get(fee_item(4))
            .is_some(),
        "the menu went out with the new pizza"
    );
}

/// The fee publish checks a store's rules against the tree it writes over (the race found in 2b):
/// a tax table that no longer rates the class a rule is taxed at, published between the fee
/// publish's read and its write, is the table the rule is checked against. The store is refused
/// and keeps the fees it had, rather than being sent a rule every bill it applies to would fail.
#[tokio::test]
async fn a_fee_publish_checks_the_rules_against_the_tree_it_writes_over() {
    let (router, config_trees, catalog, menu_id) = menu_fees_app();
    let cookie = admin_cookie(&router).await;
    let [first, ..] = fees_stores();
    let fee_id = write_pizza_packaging(&router, &cookie).await;
    let held = fee_store_tree(&config_trees, first).await.layers[2]["fees"].clone();
    // A pizza added to the category, so the publish has something to send.
    add_pizza(&catalog, menu_id, 4);
    *config_trees.interpose.lock().expect("lock") = Some(("tax".to_owned(), second_class_only()));

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees/publish",
            &serde_json::json!({
                "tenant_id": tenant_scope_id(),
                "store_id": first.to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let body = json_body(published).await;
    assert_eq!(
        outcomes(&body),
        vec![(first.to_string(), "FEE_PUBLISH_REFUSED".to_owned())],
        "{body}"
    );
    assert_eq!(
        body["stores"][0]["faults"],
        serde_json::json!([{ "fee_id": fee_id, "reason": "TAX_RATE_NOT_CONFIGURED" }])
    );
    let tree = fee_store_tree(&config_trees, first).await;
    assert_eq!(tree.layers[2]["tax"], second_class_only());
    assert_eq!(
        tree.layers[2]["fees"], held,
        "the store keeps the fees it had"
    );
}

/// A menu published to a store group brings each member's fees along in the member's own version,
/// composed on its own tree, and a member that cannot apply one of its rules still gets its menu,
/// its report saying why in the fee publish's tokens.
#[tokio::test]
async fn a_menu_batch_brings_each_members_fees_along_and_says_which_it_left_behind() {
    let [first, second, _] = fees_stores();
    let config_trees = FakeConfigTrees::default();
    seed_fee_stores(&config_trees);
    let mut tree = fee_store_tree(&config_trees, first).await;
    tree.layers[2]["tax"] = second_class_only();
    config_trees.seed_store_layer(tenant(), second, tree.layers[2].clone());
    let catalog = fee_catalog();
    let menu_id = seed_fee_menu(&catalog);

    // The brand's packaging rule, as a write the second store came to refuse left it.
    let fee_id = FeeId::new(Ulid::from_u128(0x7F11));
    let mut authored = pizza_packaging();
    authored["fee_id"] = serde_json::json!(fee_id.to_string());
    let (rule, item_category_ids) =
        pos_cloud::fees::rule_from_authored(&authored.to_string()).expect("a rule");
    let rules = InMemoryFeeRules::new();
    rules
        .put(
            tenant(),
            &FeeRule {
                scope: FeeScope::Brand,
                scope_id: fees_brand().to_string(),
                rule,
                item_category_ids,
                update_time: Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("a time"),
                updated_by: "admin-1".to_owned(),
            },
        )
        .await
        .expect("put the rule");

    let group_id = StoreGroupId::new(Ulid::from_u128(0x7F21));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id: tenant(),
            name: "The brand".to_owned(),
            status: EntityStatus::Active,
        },
        &[first, second],
    );
    let router = store_group_app_with_fees(
        provisioned_admin(),
        groups,
        fees_registry(),
        config_trees.clone(),
        catalog,
        rules,
    );
    let cookie = admin_cookie(&router).await;
    let published = router
        .oneshot(post_with_cookie(
            &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
            &serde_json::json!({
                "tenant_id": tenant_scope_id(),
                "node": "menu",
                "arguments": { "menu_id": menu_id.as_ulid().to_string() },
            }),
            &cookie,
        ))
        .await
        .expect("route the batch publish");
    assert_eq!(published.status(), StatusCode::OK);
    let report = json_body(published).await;
    assert_eq!(report["applied"], 2, "{report}");
    let row = |store: StoreId| {
        report["results"]
            .as_array()
            .expect("results")
            .iter()
            .find(|row| row["store_id"] == store.to_string())
            .cloned()
            .expect("a row")
    };

    let took_them = row(first);
    assert!(took_them.get("detail").is_none(), "{took_them}");
    let tree = fee_store_tree(&config_trees, first).await;
    assert_eq!(last_written(&tree), ["menu", "layout", "fees"]);
    assert_eq!(
        took_them["config_version_id"],
        tree.history.last().expect("a version").id.to_string()
    );
    let fees = store_fees(&config_trees, first).await.expect("a fees node");
    assert_eq!(
        fees.fees.first().map(|fee| fee.menu_item_ids.clone()),
        Some(vec![fee_item(1), fee_item(2)])
    );

    let left_them = row(second);
    let detail = left_them["detail"].as_str().expect("a detail");
    assert!(
        detail.contains("FEE_PUBLISH_REFUSED")
            && detail.contains(&format!("{fee_id} TAX_RATE_NOT_CONFIGURED")),
        "{detail}"
    );
    let tree = fee_store_tree(&config_trees, second).await;
    assert_eq!(last_written(&tree), ["menu", "layout"]);
    assert!(
        tree.layers[2].get("fees").is_none(),
        "nothing it cannot apply"
    );
}
