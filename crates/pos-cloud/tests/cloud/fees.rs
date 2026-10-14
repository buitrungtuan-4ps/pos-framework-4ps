// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The fee routes (`/admin/fees`, ADR-0159 decision 1): writing a rule, where it reaches, and
//! what is refused; and the fixtures the other fee tests share.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::AuditSink;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::catalog::{CatalogItem, ItemCategory, ItemCategoryId, TaxClass};
use pos_cloud::config_tree::ConfigTreeStore;
use pos_cloud::fees::InMemoryFeeRules;
use pos_cloud::registry::{BrandRecord, EntityStatus, StoreRecord};
use pos_cloud::version::{Version, Versioned};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::enums::SalesChannel;
use pos_proto::fees::PublishedFees;
use pos_proto::ids::{MenuItemId, StoreId, TaxClassId};
use pos_proto::locale::TaxRate;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeCatalog, FakeConfigTrees, FakeKeys, FakeRegistry, FakeRollups,
    FakeWebhooks, admin_cookie, app_all, clock, delete_with_cookie, get_with_cookie, json_body,
    outcomes, post_with_cookie, provisioned_admin, put_with_cookie, role_session_cookie, tenant,
};

// --- Fees (ADR-0159 decision 1) -----------------------------------------------------------------

pub(super) fn fees_brand() -> pos_cloud::registry::BrandId {
    pos_cloud::registry::BrandId::new(Ulid::from_u128(0x7A01))
}

/// Two stores of the brand, and a third that no brand holds.
pub(super) fn fees_stores() -> [StoreId; 3] {
    [
        StoreId::new(Ulid::from_u128(0x7B01)),
        StoreId::new(Ulid::from_u128(0x7B02)),
        StoreId::new(Ulid::from_u128(0x7B03)),
    ]
}

pub(super) fn fee_item(n: u128) -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(0x7C00 + n))
}

pub(super) fn fee_category(n: u128) -> ItemCategoryId {
    ItemCategoryId::new(Ulid::from_u128(0x7D00 + n))
}

pub(super) fn fee_tax_class(n: u128) -> TaxClassId {
    TaxClassId::new(Ulid::from_u128(0x7E00 + n))
}

/// Three items, two of them in the one category, and two tax classes.
pub(super) fn fee_catalog() -> FakeCatalog {
    let catalog = FakeCatalog::default();
    for (n, category) in [
        (1, Some(fee_category(1))),
        (2, Some(fee_category(1))),
        (3, None),
    ] {
        catalog.items.lock().expect("lock").push(Versioned::new(
            CatalogItem {
                menu_item_id: fee_item(n),
                tenant_id: tenant(),
                name: format!("Item {n}"),
                name_translations: BTreeMap::new(),
                tax_class_id: fee_tax_class(1),
                item_category_id: category,
                item_subcategory_id: None,
                course_id: None,
                image_ref: None,
                status: EntityStatus::Active,
            },
            Version::new("1"),
        ));
    }
    catalog
        .categories
        .lock()
        .expect("lock")
        .push(Versioned::new(
            ItemCategory {
                item_category_id: fee_category(1),
                tenant_id: tenant(),
                name: "Pizza".to_owned(),
                status: EntityStatus::Active,
            },
            Version::new("1"),
        ));
    for n in [1, 2] {
        catalog
            .tax_classes
            .lock()
            .expect("lock")
            .push(Versioned::new(
                TaxClass {
                    tax_class_id: fee_tax_class(n),
                    tenant_id: tenant(),
                    name: format!("Class {n}"),
                    status: EntityStatus::Active,
                },
                Version::new("1"),
            ));
    }
    catalog
}

/// The brand's first store bills in đồng, with a menu of two items and a tax table for one class;
/// its second bills in yen; the third has nothing published.
pub(super) fn seed_fee_stores(config_trees: &FakeConfigTrees) {
    let [first, second, _] = fees_stores();
    let vnd = pos_proto::money::CurrencyCode::VND;
    let menu = pos_proto::MenuBook::new().with(
        SalesChannel::DineIn,
        pos_proto::MenuCatalog::new()
            .with(pos_proto::MenuEntry::new(
                fee_item(1),
                DisplayName::new("Margherita"),
                pos_proto::money::Money::new(vnd, 95_000),
                fee_tax_class(1),
            ))
            .with(pos_proto::MenuEntry::new(
                fee_item(3),
                DisplayName::new("Lemonade"),
                pos_proto::money::Money::new(vnd, 30_000),
                fee_tax_class(1),
            )),
    );
    let tax = pos_proto::locale::TaxRateTable::new()
        .with(
            fee_tax_class(1),
            SalesChannel::DineIn,
            TaxRate::from_basis_points(800),
        )
        .with(
            fee_tax_class(1),
            SalesChannel::Takeaway,
            TaxRate::from_basis_points(800),
        );
    config_trees.seed_store_layer(
        tenant(),
        first,
        serde_json::json!({
            "locale": { "currency_code": "VND" },
            "tax": serde_json::to_value(&tax).expect("encode the tax table"),
            "menu": serde_json::to_value(&menu).expect("encode the menu"),
        }),
    );
    config_trees.seed_store_layer(
        tenant(),
        second,
        serde_json::json!({ "locale": { "currency_code": "JPY" } }),
    );
}

/// The brand and its three stores, the third held by no brand.
pub(super) fn fees_registry() -> FakeRegistry {
    let registry = FakeRegistry::default();
    let [first, second, third] = fees_stores();
    registry.brands.lock().expect("lock").push(Versioned::new(
        BrandRecord {
            brand_id: fees_brand(),
            tenant_id: tenant(),
            name: "Brand".to_owned(),
            status: EntityStatus::Active,
        },
        Version::new("1"),
    ));
    for (store_id, brand_id) in [
        (first, Some(fees_brand())),
        (second, Some(fees_brand())),
        (third, None),
    ] {
        registry.stores.lock().expect("lock").push(Versioned::new(
            StoreRecord {
                store_id,
                tenant_id: tenant(),
                brand_id,
                name: format!("Store {}", store_id.as_ulid()),
                status: EntityStatus::Active,
            },
            Version::new("1"),
        ));
    }
    registry
}

/// The fee routes over fakes. The brand's first store bills in đồng with a menu and a tax table,
/// its second bills in yen, and the third has nothing published yet.
pub(super) fn fees_app() -> (axum::Router, FakeConfigTrees, FakeAudit, FakeAdmin) {
    let admin = provisioned_admin();
    let config_trees = FakeConfigTrees::default();
    let registry = fees_registry();
    let catalog = fee_catalog();
    seed_fee_stores(&config_trees);
    let audit = FakeAudit::default();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    let router = http::router(app).merge(http::fees_router(
        InMemoryFeeRules::new(),
        registry,
        catalog,
        config_trees.clone(),
        admin.clone(),
        clock(),
        Arc::new(AuditSink::new(audit.clone())),
    ));
    (router, config_trees, audit, admin)
}

/// A `POST` or `PUT /admin/fees` body.
pub(super) fn fee_body(scope: &str, scope_id: &str, rule: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "scope": scope,
        "scope_id": scope_id,
        "rule": rule,
    })
}

/// A percentage of every line, as the console starts a new rule.
pub(super) fn percent_fee(code: &str, percent: i64) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "display_name": "Service charge",
        "display_name_translations": { "vi": "Phí phục vụ" },
        "kind": "FEE_KIND_PERCENT",
        "rate": { "numerator": percent, "denominator": 100 },
    })
}

/// A fee per bill in `currency`.
fn amount_fee(code: &str, currency: &str, amount: i64) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "display_name": "Delivery",
        "kind": "FEE_KIND_AMOUNT_PER_BILL",
        "amount": { "currency_code": currency, "amount_minor": amount },
    })
}

/// `rule` with its `fee_id` set, as a `PUT` sends it.
fn with_fee_id(rule: &serde_json::Value, fee_id: &str) -> serde_json::Value {
    let mut rule = rule.clone();
    if let Some(fields) = rule.as_object_mut() {
        fields.insert("fee_id".to_owned(), serde_json::json!(fee_id));
    }
    rule
}

/// The `fees` node a store's Store layer carries, parsed as the edge parses it.
pub(super) async fn store_fees(
    config_trees: &FakeConfigTrees,
    store_id: StoreId,
) -> Option<PublishedFees> {
    let tree = config_trees.load(tenant(), store_id).await.expect("load")?;
    let node = tree.record.layers[2].get("fees")?;
    serde_json::from_str(&node.to_string()).ok()
}

pub(super) fn tenant_scope_id() -> String {
    tenant().as_ulid().to_string()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one fee through its life against the same stores: written for the tenant, overridden \
              for a brand, read back per store, written again unchanged, the override removed, and \
              the list read — split, each part would rebuild the rules the next one starts from"
)]
async fn a_tenant_fee_reaches_every_store_and_a_brand_rule_for_the_same_fee_wins_at_its_stores() {
    let (router, config_trees, _, _) = fees_app();
    let cookie = admin_cookie(&router).await;
    let [first, second, third] = fees_stores();

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees",
            &fee_body(
                "FEE_SCOPE_TENANT",
                &tenant_scope_id(),
                &percent_fee("SERVICE", 5),
            ),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(created.status(), StatusCode::CREATED);
    let body = json_body(created).await;
    let fee_id = body["fee_id"].as_str().expect("a minted fee id").to_owned();
    assert_eq!(
        outcomes(&body),
        vec![
            (first.to_string(), "FEE_PUBLISH_APPLIED".to_owned()),
            (second.to_string(), "FEE_PUBLISH_APPLIED".to_owned()),
            (third.to_string(), "FEE_PUBLISH_APPLIED".to_owned()),
        ]
    );
    for store_id in fees_stores() {
        let fees = store_fees(&config_trees, store_id)
            .await
            .expect("a fees node");
        let [fee] = fees.fees.as_slice() else {
            panic!("one fee at {store_id}: {fees:?}");
        };
        assert_eq!(fee.fee_id.to_string(), fee_id);
        assert_eq!(fee.localized_name("vi").as_str(), "Phí phục vụ");
    }

    // The same fee, written for the brand at 10 %, replaces the tenant's at the brand's stores.
    let brand = fees_brand().to_string();
    let overridden = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/fees",
            &fee_body(
                "FEE_SCOPE_BRAND",
                &brand,
                &with_fee_id(&percent_fee("SERVICE", 10), &fee_id),
            ),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(overridden.status(), StatusCode::OK);
    assert_eq!(
        outcomes(&json_body(overridden).await),
        vec![
            (first.to_string(), "FEE_PUBLISH_APPLIED".to_owned()),
            (second.to_string(), "FEE_PUBLISH_APPLIED".to_owned()),
        ],
        "the brand's two stores, and not the third"
    );
    let rate_at = |fees: &PublishedFees| fees.fees.first().and_then(|fee| fee.rate);
    let first_fees = store_fees(&config_trees, first).await.expect("a fees node");
    let third_fees = store_fees(&config_trees, third).await.expect("a fees node");
    assert_eq!(
        rate_at(&first_fees).map(pos_proto::money::Ratio::numerator),
        Some(10)
    );
    assert_eq!(
        rate_at(&third_fees).map(pos_proto::money::Ratio::numerator),
        Some(5)
    );

    // Where each store's fee comes from.
    let effective = |store_id: StoreId| {
        get_with_cookie(
            &format!(
                "/admin/fees/effective?tenant_id={}&store_id={store_id}",
                tenant().as_ulid()
            ),
            &cookie,
        )
    };
    let first_runs = json_body(
        router
            .clone()
            .oneshot(effective(first))
            .await
            .expect("route"),
    )
    .await;
    assert_eq!(first_runs["fees"][0]["scope"], "FEE_SCOPE_BRAND");
    assert_eq!(first_runs["fees"][0]["published"]["rate"]["numerator"], 10);
    assert_eq!(first_runs["publishable"], true);
    let third_runs = json_body(
        router
            .clone()
            .oneshot(effective(third))
            .await
            .expect("route"),
    )
    .await;
    assert_eq!(third_runs["fees"][0]["scope"], "FEE_SCOPE_TENANT");

    // Written again unchanged, no store is given a new version.
    let again = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/fees",
            &fee_body(
                "FEE_SCOPE_BRAND",
                &brand,
                &with_fee_id(&percent_fee("SERVICE", 10), &fee_id),
            ),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert!(
        outcomes(&json_body(again).await)
            .iter()
            .all(|(_, outcome)| outcome == "FEE_PUBLISH_UNCHANGED")
    );

    // The brand's rule removed, its stores run the tenant's again.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!(
                "/admin/fees?tenant_id={}&scope=FEE_SCOPE_BRAND&scope_id={brand}&fee_id={fee_id}",
                tenant().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the delete");
    assert_eq!(removed.status(), StatusCode::OK);
    let first_fees = store_fees(&config_trees, first).await.expect("a fees node");
    assert_eq!(
        rate_at(&first_fees).map(pos_proto::money::Ratio::numerator),
        Some(5)
    );

    let listed = json_body(
        router
            .oneshot(get_with_cookie(
                &format!("/admin/fees?tenant_id={}", tenant().as_ulid()),
                &cookie,
            ))
            .await
            .expect("route the read"),
    )
    .await;
    let rules = listed["rules"].as_array().expect("a list of rules");
    assert_eq!(
        rules.len(),
        1,
        "the tenant's rule, and the brand's no longer"
    );
    assert_eq!(rules[0]["scope"], "FEE_SCOPE_TENANT");
    assert_eq!(rules[0]["rule"]["fee_id"], fee_id.as_str());
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "each refusal a store's configuration causes, against the same stores: a currency on \
              a write and on a delete that would uncover it, a tax class, and a publish to a store \
              that changed currency after its fees reached it"
)]
async fn a_rule_a_store_could_not_apply_is_refused_and_a_store_that_cannot_keeps_its_fees() {
    let (router, config_trees, _, _) = fees_app();
    let cookie = admin_cookie(&router).await;
    let [first, second, third] = fees_stores();

    // A delivery fee in đồng reaches the brand's second store, which bills in yen.
    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees",
            &fee_body(
                "FEE_SCOPE_TENANT",
                &tenant_scope_id(),
                &amount_fee("DELIVERY", "VND", 15_000),
            ),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "rule.amount");
    assert_eq!(body["error"]["details"][0]["reason"], "CURRENCY_MISMATCH");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(&second.to_string())),
        "the refusal names the store by id: {body}"
    );
    assert!(
        store_fees(&config_trees, first).await.is_none(),
        "nothing was written"
    );

    // The yen store given its own rule for the fee first, the đồng rule applies everywhere else.
    let fee_id = Ulid::from_u128(0x7F01).to_string();
    for (scope, scope_id, rule) in [
        (
            "FEE_SCOPE_STORE",
            second.to_string(),
            amount_fee("DELIVERY", "JPY", 300),
        ),
        (
            "FEE_SCOPE_TENANT",
            tenant_scope_id(),
            amount_fee("DELIVERY", "VND", 15_000),
        ),
    ] {
        let written = router
            .clone()
            .oneshot(put_with_cookie(
                "/admin/fees",
                &fee_body(scope, &scope_id, &with_fee_id(&rule, &fee_id)),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(written.status(), StatusCode::OK, "{scope}");
    }

    // Removing the yen rule would leave the yen store charging đồng.
    let uncovering = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!(
                "/admin/fees?tenant_id={}&scope=FEE_SCOPE_STORE&scope_id={second}&fee_id={fee_id}",
                tenant().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the delete");
    assert_eq!(uncovering.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(uncovering).await;
    assert_eq!(body["error"]["details"][0]["field"], "fee_id");
    assert_eq!(body["error"]["details"][0]["reason"], "CURRENCY_MISMATCH");

    // A fee taxed at a class the first store has no rate for.
    let taxed = serde_json::json!({
        "code": "SERVICE",
        "display_name": "Service charge",
        "kind": "FEE_KIND_PERCENT",
        "rate": { "numerator": 5, "denominator": 100 },
        "tax": "FEE_TAX_TAX_CLASS",
        "tax_class_id": fee_tax_class(2).to_string(),
    });
    let unrated = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees",
            &fee_body("FEE_SCOPE_STORE", &first.to_string(), &taxed),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(unrated.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(unrated).await;
    assert_eq!(body["error"]["details"][0]["field"], "rule.tax_class_id");
    assert_eq!(
        body["error"]["details"][0]["reason"],
        "TAX_RATE_NOT_CONFIGURED"
    );

    // A store that comes to bill in yen after the đồng rule reached it is not sent its fees again,
    // and keeps the ones it has.
    config_trees.seed_store_layer(
        tenant(),
        third,
        serde_json::json!({ "locale": { "currency_code": "JPY" } }),
    );
    let published = router
        .oneshot(post_with_cookie(
            "/admin/fees/publish",
            &serde_json::json!({ "tenant_id": tenant_scope_id() }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let body = json_body(published).await;
    let stores = body["stores"].as_array().expect("a list of stores");
    let third_result = stores
        .iter()
        .find(|store| store["store_id"] == third.to_string())
        .expect("the third store");
    assert_eq!(third_result["outcome"], "FEE_PUBLISH_REFUSED");
    assert_eq!(third_result["faults"][0]["fee_id"], fee_id.as_str());
    assert_eq!(third_result["faults"][0]["reason"], "CURRENCY_MISMATCH");
    assert!(store_fees(&config_trees, third).await.is_none());
}

#[tokio::test]
async fn a_rule_is_refused_field_by_field() {
    let (router, _, _, _) = fees_app();
    let cookie = admin_cookie(&router).await;
    let tenant_id = tenant_scope_id();
    let unknown_item = serde_json::json!({
        "code": "PACKAGING",
        "display_name": "Packaging",
        "kind": "FEE_KIND_AMOUNT_PER_UNIT",
        "amount": { "currency_code": "VND", "amount_minor": 3_000 },
        "item_scope": "FEE_ITEMS_INCLUDE",
        "menu_item_ids": [Ulid::from_u128(0x7C99).to_string()],
    });
    for (scope, rule, field, reason) in [
        (
            "FEE_SCOPE_TENANT",
            percent_fee("SERVICE", 150),
            "rule.rate",
            "OUT_OF_RANGE",
        ),
        (
            "FEE_SCOPE_TENANT",
            serde_json::json!({
                "code": "SERVICE",
                "display_name": "Service charge",
                "kind": "FEE_KIND_PERCENT",
            }),
            "rule.rate",
            "REQUIRED",
        ),
        (
            "FEE_SCOPE_TENANT",
            serde_json::json!({
                "code": "COVER",
                "display_name": "Cover charge",
                "kind": "FEE_KIND_PER_GUEST",
            }),
            "rule.kind",
            "INVALID_ENUM_VALUE",
        ),
        (
            "FEE_SCOPE_TENANT",
            serde_json::json!({ "code": " ", "display_name": "Service charge" }),
            "rule.code",
            "REQUIRED",
        ),
        (
            "FEE_SCOPE_TENANT",
            unknown_item,
            "rule.menu_item_ids",
            "UNKNOWN_REFERENCE",
        ),
        (
            "FEE_SCOPE_TENANT",
            with_fee_id(&percent_fee("SERVICE", 5), &Ulid::from_u128(1).to_string()),
            "rule.fee_id",
            "INVALID_VALUE",
        ),
        (
            "FEE_SCOPE_STORE_GROUP",
            percent_fee("SERVICE", 5),
            "scope",
            "INVALID_ENUM_VALUE",
        ),
    ] {
        let refused = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/fees",
                &fee_body(scope, &tenant_id, &rule),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "{field} {reason}"
        );
        let body = json_body(refused).await;
        assert_eq!(body["error"]["details"][0]["field"], field, "{body}");
        assert_eq!(body["error"]["details"][0]["reason"], reason, "{body}");
    }
}

#[tokio::test]
async fn a_category_is_compiled_into_its_items_and_a_list_must_name_one_the_menu_has() {
    let (router, config_trees, _, _) = fees_app();
    let cookie = admin_cookie(&router).await;
    let [first, ..] = fees_stores();

    let packaging = serde_json::json!({
        "code": "PACKAGING",
        "display_name": "Packaging",
        "kind": "FEE_KIND_AMOUNT_PER_UNIT",
        "amount": { "currency_code": "VND", "amount_minor": 3_000 },
        "channels": ["SALES_CHANNEL_TAKEAWAY", "SALES_CHANNEL_DINE_IN"],
        "item_scope": "FEE_ITEMS_INCLUDE",
        "item_category_ids": [fee_category(1).to_string()],
    });
    let written = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees",
            &fee_body("FEE_SCOPE_STORE", &first.to_string(), &packaging),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::CREATED);
    let fees = store_fees(&config_trees, first).await.expect("a fees node");
    let [fee] = fees.fees.as_slice() else {
        panic!("one fee: {fees:?}");
    };
    assert_eq!(
        fee.menu_item_ids,
        vec![fee_item(1), fee_item(2)],
        "the category's two items, compiled when the store was published"
    );

    // A second fee under the code the store already runs would be charged as a second fee.
    let clash = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/fees",
            &fee_body("FEE_SCOPE_TENANT", &tenant_scope_id(), &{
                let mut rule = percent_fee("PACKAGING", 5);
                if let Some(fields) = rule.as_object_mut() {
                    fields.insert("display_name".to_owned(), serde_json::json!("Packaging"));
                }
                rule
            }),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(clash.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(clash).await;
    assert_eq!(body["error"]["details"][0]["field"], "rule.code");
    assert_eq!(body["error"]["details"][0]["reason"], "ALREADY_EXISTS");

    // An include list of an item no store it reaches has on its menu.
    let nothing_on_the_menu = serde_json::json!({
        "code": "BOX",
        "display_name": "Box",
        "kind": "FEE_KIND_AMOUNT_PER_UNIT",
        "amount": { "currency_code": "VND", "amount_minor": 2_000 },
        "item_scope": "FEE_ITEMS_INCLUDE",
        "menu_item_ids": [fee_item(2).to_string()],
    });
    let refused = router
        .oneshot(post_with_cookie(
            "/admin/fees",
            &fee_body("FEE_SCOPE_STORE", &first.to_string(), &nothing_on_the_menu),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "rule.menu_item_ids");
    assert_eq!(body["error"]["details"][0]["reason"], "NOT_ON_MENU");
}

#[tokio::test]
async fn the_fee_routes_go_by_role_and_the_trail_keeps_ids_and_counts() {
    let (router, _, audit, admin) = fees_app();
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let list = format!("/admin/fees?tenant_id={}", tenant().as_ulid());

    // A rule's rate and amount are prices: neither Ops nor a Viewer reads them.
    for cookie in [&ops, &viewer] {
        let read = router
            .clone()
            .oneshot(get_with_cookie(&list, cookie))
            .await
            .expect("route the read");
        assert_eq!(read.status(), StatusCode::FORBIDDEN);
    }
    // A write publishes, which Ops may do and a Viewer may not.
    let body = fee_body(
        "FEE_SCOPE_TENANT",
        &tenant_scope_id(),
        &percent_fee("SERVICE", 5),
    );
    let by_viewer = router
        .clone()
        .oneshot(post_with_cookie("/admin/fees", &body, &viewer))
        .await
        .expect("route the write");
    assert_eq!(by_viewer.status(), StatusCode::FORBIDDEN);
    let by_ops = router
        .clone()
        .oneshot(post_with_cookie("/admin/fees", &body, &ops))
        .await
        .expect("route the write");
    assert_eq!(by_ops.status(), StatusCode::CREATED);
    let fee_id = json_body(by_ops).await["fee_id"]
        .as_str()
        .expect("a fee id")
        .to_owned();

    let entries = audit.entries.lock().expect("lock").clone();
    let [entry] = entries.as_slice() else {
        panic!("one audit entry: {entries:?}");
    };
    assert_eq!(entry.action, "fee.create");
    assert_eq!(entry.entity_type, "fee_rule");
    assert_eq!(
        entry.entity_id,
        format!("{fee_id} FEE_SCOPE_TENANT {}", tenant().as_ulid())
    );
    assert_eq!(
        entry.after,
        Some(serde_json::json!({
            "stores": 3, "applied": 3, "unchanged": 0, "refused": 0, "failed": 0,
        }))
    );
    let recorded = serde_json::to_string(&entry.after).expect("encode");
    assert!(
        !recorded.contains("SERVICE") && !recorded.contains("Service charge"),
        "ids and counts only: {recorded}"
    );
}
