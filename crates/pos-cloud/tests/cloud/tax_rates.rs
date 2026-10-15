// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The tax-rate routes (`/admin/catalog/tax-rates`, ADR-0074): rates, components and their checks.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::catalog::TaxClass;
use pos_cloud::registry::EntityStatus;
use pos_cloud::tax::TaxRateEntry;
use pos_cloud::version::{Version, Versioned};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{TaxClassId, TenantId};
use pos_proto::locale::{TaxComponent, TaxRate};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeCatalog, FakeConfigTrees, FakeKeys, FakeRollups, FakeTaxRates, FakeWebhooks,
    admin_cookie, app_all, clock, etag_of, get, get_with_cookie, json_body, provisioned_admin,
    put_with_etag, ulid_text,
};

fn tax_rate_app(admin: FakeAdmin, catalog: FakeCatalog, tax_rates: FakeTaxRates) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::tax_rate_router(
        tax_rates,
        catalog,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// Seeds a tax class straight into the fake, so a rate can name a known class without the catalog
/// create route.
fn seed_tax_class(catalog: &FakeCatalog, tenant: &str, tax_class_id: &str, name: &str) {
    catalog
        .tax_classes
        .lock()
        .expect("lock")
        .push(Versioned::new(
            TaxClass {
                tax_class_id: tax_class_id
                    .parse::<Ulid>()
                    .map(TaxClassId::new)
                    .expect("a class ULID"),
                tenant_id: tenant
                    .parse::<Ulid>()
                    .map(TenantId::new)
                    .expect("a tenant ULID"),
                name: name.to_owned(),
                status: EntityStatus::Active,
            },
            // Seeded straight in, so it starts at a version nothing has read yet.
            catalog.mint(),
        ));
}

#[tokio::test]
async fn tax_rates_set_lists_and_validates() {
    let catalog = FakeCatalog::default();
    let tenant = ulid_text(1);
    let class = ulid_text(7);
    seed_tax_class(&catalog, &tenant, &class, "Standard");
    let router = tax_rate_app(provisioned_admin(), catalog, FakeTaxRates::default());
    let cookie = admin_cookie(&router).await;

    // PUT the tenant's table: one class taxed 10% dine-in and 8% takeaway. `If-Match: *` is how a
    // tenant that has never saved rates says so (ADR-0095); the response carries the new version.
    let set = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 1000 },
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_TAKEAWAY", "rate_bps": 800 },
            ] }),
            &cookie,
            "*",
        ))
        .await
        .expect("route set tax rates");
    assert_eq!(set.status(), StatusCode::OK);
    let after_set = etag_of(&set);

    // GET reads them back, and hands out the same version the save returned.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list tax rates");
    assert_eq!(listed.status(), StatusCode::OK);
    let current = etag_of(&listed);
    assert_eq!(
        current, after_set,
        "the version a save returns is the one the next read hands out"
    );
    let rows = json_body(listed).await;
    assert_eq!(rows.as_array().expect("array").len(), 2);

    // A rate naming an unknown class is a 400, not a store error.
    let bad_class = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": ulid_text(99), "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 1000 },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route unknown class");
    assert_eq!(bad_class.status(), StatusCode::BAD_REQUEST);

    // A rate above 100% is a 400.
    let bad_rate = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 10_001 },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route bad rate");
    assert_eq!(bad_rate.status(), StatusCode::BAD_REQUEST);
}

/// An Indian rate authored as CGST + SGST survives the round trip, and one whose parts miss the
/// total is refused where it is typed ([ADR-0104](../../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
#[tokio::test]
async fn tax_rate_components_round_trip_and_must_sum_to_their_rate() {
    let catalog = FakeCatalog::default();
    let tenant = ulid_text(1);
    let class = ulid_text(7);
    seed_tax_class(&catalog, &tenant, &class, "Food");
    let router = tax_rate_app(provisioned_admin(), catalog, FakeTaxRates::default());
    let cookie = admin_cookie(&router).await;

    // 5% restaurant GST as the two halves that go to different governments.
    let saved = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 500,
                  "components": [
                      { "name": "CGST", "rate_bps": 250 },
                      { "name": "SGST", "rate_bps": 250 },
                  ] },
            ] }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the components save");
    assert_eq!(saved.status(), StatusCode::OK);
    let current = etag_of(&saved);

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route list tax rates");
    let rows = json_body(listed).await;
    let components = rows
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("components"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .expect("the row carries its breakdown");
    assert_eq!(components.len(), 2);
    assert_eq!(
        components.first().and_then(|c| c.get("name")),
        Some(&serde_json::json!("CGST"))
    );
    assert_eq!(
        components.first().and_then(|c| c.get("rate_bps")),
        Some(&serde_json::json!(250))
    );

    // Parts that miss their own rate would print an invoice whose lines do not add up to the tax
    // charged. Refused here, where it is authored, rather than discovered on a document.
    let unbalanced = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 500,
                  "components": [
                      { "name": "CGST", "rate_bps": 250 },
                      { "name": "SGST", "rate_bps": 200 },
                  ] },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route the unbalanced save");
    assert_eq!(unbalanced.status(), StatusCode::BAD_REQUEST);

    // A component with no name is refused too: the name is the whole point — it is what the invoice
    // prints, and an unnamed line is not a line an auditor can read.
    let nameless = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 500,
                  "components": [{ "name": "   ", "rate_bps": 500 }] },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route the nameless save");
    assert_eq!(nameless.status(), StatusCode::BAD_REQUEST);

    // And a grid saved with no `components` key at all still saves — the console that predates
    // ADR-0104 is not broken by it.
    let legacy = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 1000 },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route the legacy save");
    assert_eq!(legacy.status(), StatusCode::OK);
}

/// A component's name is a token, or its row is refused where it is typed, by its class and channel
/// and with the rule ([ADR-0168](../../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md)
/// decision 2).
#[tokio::test]
async fn a_tax_component_name_that_is_not_a_token_is_refused_where_it_is_typed() {
    let catalog = FakeCatalog::default();
    let tenant = ulid_text(1);
    let class = ulid_text(7);
    seed_tax_class(&catalog, &tenant, &class, "Food");
    let router = tax_rate_app(provisioned_admin(), catalog, FakeTaxRates::default());
    let cookie = admin_cookie(&router).await;
    let grid = |name: &str| {
        serde_json::json!({ "tenant_id": tenant, "rates": [
            { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 500,
              "components": [{ "name": name, "rate_bps": 250 }, { "name": "SGST", "rate_bps": 250 }] },
            // A Vietnamese row, which names no component, passes whatever its neighbour names.
            { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_TAKEAWAY", "rate_bps": 800 },
        ] })
    };

    for name in ["Central GST", "cgst"] {
        let refused = router
            .clone()
            .oneshot(put_with_etag(
                "/admin/catalog/tax-rates",
                &grid(name),
                &cookie,
                "*",
            ))
            .await
            .expect("route the save");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{name}");
        let body = json_body(refused).await;
        assert_eq!(
            body["error"]["details"],
            serde_json::json!([{ "field": "rates", "reason": "INVALID_FORMAT" }]),
            "{name}"
        );
        let message = body["error"]["message"].as_str().expect("a message");
        assert!(
            message.contains(&class)
                && message.contains("SALES_CHANNEL_DINE_IN")
                && message.contains("2 to 8 upper-case letters and digits, starting with a letter"),
            "the refusal names the row and the rule: {message}"
        );
        assert!(!message.contains(name), "and not the name: {message}");
    }

    // A token passes, and so does one typed with spaces around it, which is stored trimmed.
    let saved = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &grid(" CGST "),
            &cookie,
            "*",
        ))
        .await
        .expect("route the save");
    assert_eq!(saved.status(), StatusCode::OK);
    let current = etag_of(&saved);
    let resaved = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &grid("CGST"),
            &cookie,
            &current,
        ))
        .await
        .expect("route the save");
    assert_eq!(resaved.status(), StatusCode::OK);
    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the read");
    let rows = json_body(listed).await;
    assert_eq!(rows[0]["components"][0]["name"], "CGST");
    assert!(
        rows.as_array()
            .expect("rows")
            .iter()
            .all(|row| row.get("component_name_invalid").is_none()),
        "a sound grid reads as it always did: {rows}"
    );
}

/// A row saved before names were checked is kept as it was saved and marked when it is read, and
/// the grid holding it cannot be saved again until the name changes (ADR-0168 decision 2).
#[tokio::test]
async fn a_stored_row_named_otherwise_is_kept_marked_and_refuses_an_unchanged_save() {
    let catalog = FakeCatalog::default();
    let tenant = ulid_text(1);
    let class = ulid_text(7);
    seed_tax_class(&catalog, &tenant, &class, "Food");
    let tenant_id = TenantId::new(tenant.parse::<Ulid>().expect("a tenant ULID"));
    let class_id = TaxClassId::new(class.parse::<Ulid>().expect("a class ULID"));
    let (gst, half) = (
        TaxRate::from_basis_points(500),
        TaxRate::from_basis_points(250),
    );
    let before = vec![
        TaxRateEntry::new(class_id, SalesChannel::DineIn, gst).with_components(vec![
            TaxComponent::new("Central GST", half),
            TaxComponent::new("SGST", half),
        ]),
        TaxRateEntry::new(class_id, SalesChannel::Takeaway, gst).with_components(vec![
            TaxComponent::new("CGST", half),
            TaxComponent::new("SGST", half),
        ]),
    ];
    // Written straight to the store, as the grid was saved before the rule.
    let tax_rates = FakeTaxRates::default();
    tax_rates
        .rows
        .lock()
        .expect("lock")
        .extend(before.iter().map(|entry| (tenant_id, entry.clone())));
    tax_rates
        .versions
        .lock()
        .expect("lock")
        .insert(tenant_id, Version::new("tx-before"));
    let stored = || -> Vec<TaxRateEntry> {
        let rows = tax_rates.rows.lock().expect("lock");
        rows.iter().map(|(_owner, entry)| entry.clone()).collect()
    };
    let router = tax_rate_app(provisioned_admin(), catalog, tax_rates.clone());
    let cookie = admin_cookie(&router).await;

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(listed.status(), StatusCode::OK);
    let current = etag_of(&listed);
    let rows = json_body(listed).await;
    assert_eq!(
        (
            &rows[0]["component_name_invalid"],
            &rows[0]["components"][0]["name"]
        ),
        (&serde_json::json!(true), &serde_json::json!("Central GST")),
        "the row is marked, and reads as it was saved: {rows}"
    );
    assert!(
        rows[1].get("component_name_invalid").is_none(),
        "a sound row carries no mark: {rows}"
    );
    assert_eq!(
        (stored(), current.as_str()),
        (before.clone(), "tx-before"),
        "and reading it rewrote nothing"
    );

    // The grid sent back as it was read, mark and all, is refused: the mark is output only, and a
    // save replaces the whole grid.
    let unchanged = serde_json::json!({ "tenant_id": tenant, "rates": rows });
    let refused = router
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &unchanged,
            &cookie,
            &current,
        ))
        .await
        .expect("route the save");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(refused).await["error"]["details"],
        serde_json::json!([{ "field": "rates", "reason": "INVALID_FORMAT" }])
    );
    assert_eq!(stored(), before, "and the refused save changed nothing");
}

/// A save against a version the grid no longer holds is refused, and changes nothing (ADR-0095).
#[tokio::test]
async fn a_tax_rate_save_against_a_stale_version_is_refused() {
    let catalog = FakeCatalog::default();
    let tenant = ulid_text(1);
    let class = ulid_text(7);
    seed_tax_class(&catalog, &tenant, &class, "Standard");
    let router = tax_rate_app(provisioned_admin(), catalog, FakeTaxRates::default());
    let cookie = admin_cookie(&router).await;

    let seeded = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 1000 },
            ] }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the first save");
    assert_eq!(seeded.status(), StatusCode::OK);
    let current = etag_of(&seeded);

    // A save at the current version applies; replaying that version afterwards is the lost update
    // the collection version exists to refuse, and the winning grid survives it.
    let winner = serde_json::json!({ "tenant_id": tenant, "rates": [
        { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 500 },
    ] });
    let applied = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &winner,
            &cookie,
            &current,
        ))
        .await
        .expect("route the winning save");
    assert_eq!(applied.status(), StatusCode::OK);

    let replayed = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &serde_json::json!({ "tenant_id": tenant, "rates": [
                { "tax_class_id": class, "sales_channel": "SALES_CHANNEL_DINE_IN", "rate_bps": 9_900 },
            ] }),
            &cookie,
            &current,
        ))
        .await
        .expect("route the stale save");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);

    // And `*` after the table exists is the same false claim the config tree refuses.
    let stale_wildcard = router
        .clone()
        .oneshot(put_with_etag(
            "/admin/catalog/tax-rates",
            &winner,
            &cookie,
            "*",
        ))
        .await
        .expect("route the wildcard save");
    assert_eq!(stale_wildcard.status(), StatusCode::PRECONDITION_FAILED);

    let survived = router
        .oneshot(get_with_cookie(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the final read");
    let final_rows = json_body(survived).await;
    assert_eq!(
        final_rows.as_array().expect("array").len(),
        1,
        "no refused save changed the table"
    );
    assert_eq!(final_rows[0]["rate_bps"], 500);
}

#[tokio::test]
async fn tax_rate_routes_require_a_session() {
    let router = tax_rate_app(
        provisioned_admin(),
        FakeCatalog::default(),
        FakeTaxRates::default(),
    );
    let tenant = ulid_text(1);
    let listed = router
        .oneshot(get(
            &format!("/admin/catalog/tax-rates?tenant_id={tenant}"),
            None,
        ))
        .await
        .expect("route");
    assert_eq!(listed.status(), StatusCode::UNAUTHORIZED);
}
