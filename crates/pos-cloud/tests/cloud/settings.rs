// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The settings routes (ADR-0160 decision 3): a value at a brand, a store or a group, the catalogue,
//! and the values the register offers reaching their nodes.

use axum::http::StatusCode;
use pos_cloud::config_tree::ConfigTreeStore;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, NO_SHIFT_SELLING, admin_cookie, archived_group, delete_with_cookie,
    get_with_cookie, json_body, outcomes, post_with_cookie, put_with_cookie, sent_to, settings_app,
    settings_brand, settings_group, settings_stores, tenant, tenant_layer_node,
    tenant_layer_session,
};

fn put_setting(scope: &str, scope_id: &str, value: &str) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "setting_key": NO_SHIFT_SELLING,
        "scope": scope,
        "scope_id": scope_id,
        "value": value,
    })
}

/// The value a store's Tenant layer carries for `shift.no_shift_selling`, if any.
async fn tenant_layer_shift(config_trees: &FakeConfigTrees, store_id: StoreId) -> Option<String> {
    let tree = config_trees.load(tenant(), store_id).await.expect("load")?;
    tree.record.layers[0]["shift"]["no_shift_selling"]
        .as_str()
        .map(str::to_owned)
}

#[tokio::test]
async fn a_brand_value_reaches_the_brands_stores_on_their_tenant_layer_and_no_other() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, second, third] = settings_stores();
    let brand = settings_brand().to_string();

    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &put_setting("SETTING_SCOPE_BRAND", &brand, "NO_SHIFT_SELLING_REFUSE"),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    let body = json_body(written).await;
    assert_eq!(
        outcomes(&body),
        vec![
            (first.to_string(), "SETTING_PUBLISH_APPLIED".to_owned()),
            (second.to_string(), "SETTING_PUBLISH_APPLIED".to_owned()),
        ],
        "the brand's two stores, and not the third"
    );
    for store_id in [first, second] {
        assert_eq!(
            tenant_layer_shift(&config_trees, store_id).await.as_deref(),
            Some("NO_SHIFT_SELLING_REFUSE")
        );
        let tree = config_trees
            .load(tenant(), store_id)
            .await
            .expect("load")
            .expect("a tree");
        assert!(
            tree.record.layers[2].get("shift").is_none(),
            "the Store layer, which node routes write, is not touched"
        );
    }
    assert_eq!(tenant_layer_shift(&config_trees, third).await, None);

    // The same write again changes nothing, so no store is given a new version.
    let again = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &put_setting("SETTING_SCOPE_BRAND", &brand, "NO_SHIFT_SELLING_REFUSE"),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert!(
        outcomes(&json_body(again).await)
            .iter()
            .all(|(_, outcome)| outcome == "SETTING_PUBLISH_UNCHANGED")
    );

    // What each store runs, and where it comes from.
    let effective = |store_id: StoreId| {
        get_with_cookie(
            &format!(
                "/admin/settings/effective?tenant_id={}&store_id={store_id}",
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
    assert_eq!(
        first_runs["settings"][0]["value"],
        "NO_SHIFT_SELLING_REFUSE"
    );
    assert_eq!(first_runs["settings"][0]["scope"], "SETTING_SCOPE_BRAND");
    let third_runs = json_body(router.oneshot(effective(third)).await.expect("route")).await;
    assert_eq!(third_runs["settings"][0]["value"], "NO_SHIFT_SELLING_ALLOW");
    assert!(
        third_runs["settings"][0].get("scope").is_none(),
        "a default comes from no scope"
    );
}

#[tokio::test]
async fn a_store_value_wins_over_its_brand_and_clearing_it_falls_back() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();

    for body in [
        put_setting(
            "SETTING_SCOPE_BRAND",
            &settings_brand().to_string(),
            "NO_SHIFT_SELLING_REFUSE",
        ),
        put_setting(
            "SETTING_SCOPE_STORE",
            &first.to_string(),
            "NO_SHIFT_SELLING_ALLOW",
        ),
    ] {
        let written = router
            .clone()
            .oneshot(put_with_cookie("/admin/settings", &body, &cookie))
            .await
            .expect("route the write");
        assert_eq!(written.status(), StatusCode::OK);
    }
    assert_eq!(
        tenant_layer_shift(&config_trees, first).await.as_deref(),
        Some("NO_SHIFT_SELLING_ALLOW"),
        "the store's own value is the more specific"
    );

    let cleared = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!(
                "/admin/settings?tenant_id={}&setting_key={NO_SHIFT_SELLING}\
                 &scope=SETTING_SCOPE_STORE&scope_id={first}",
                tenant().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the clear");
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(
        tenant_layer_shift(&config_trees, first).await.as_deref(),
        Some("NO_SHIFT_SELLING_REFUSE"),
        "the brand's value again"
    );

    let nothing_there = router
        .oneshot(delete_with_cookie(
            &format!(
                "/admin/settings?tenant_id={}&setting_key={NO_SHIFT_SELLING}\
                 &scope=SETTING_SCOPE_STORE&scope_id={first}",
                tenant().as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the clear");
    assert_eq!(nothing_there.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_new_store_is_given_the_owners_values_once() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [_, _, third] = settings_stores();
    let presets = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": third.to_string(),
    });

    let applied = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/settings/presets",
            &presets,
            &cookie,
        ))
        .await
        .expect("route the presets");
    assert_eq!(applied.status(), StatusCode::OK);
    assert_eq!(
        json_body(applied).await["applied"],
        serde_json::json!([
            NO_SHIFT_SELLING,
            "shift.drawer_model",
            "session.idle_lock_seconds",
            "permissions.enforced",
            "printing.receipt_language",
            "qr.table_order",
            "counter.walk_in_channel"
        ])
    );
    assert_eq!(
        tenant_layer_shift(&config_trees, third).await.as_deref(),
        Some("NO_SHIFT_SELLING_REFUSE"),
        "a new store refuses to sell with no shift open (the owner, 2026-09-30)"
    );
    assert_eq!(
        tenant_layer_node(&config_trees, third, "shift").await,
        Some(serde_json::json!({
            "no_shift_selling": "NO_SHIFT_SELLING_REFUSE",
            "drawer_model": "DRAWER_MODEL_PER_TERMINAL",
        })),
        "a new store keeps a drawer for each till (the owner, 2026-10-01; ADR-0167 decision 1)"
    );
    assert_eq!(
        tenant_layer_session(&config_trees, third).await,
        Some(serde_json::json!({ "idle_lock_seconds": 120 })),
        "a new store's till locks after two minutes without a touch (the owner, 2026-10-01)"
    );
    assert_eq!(
        tenant_layer_node(&config_trees, third, "permissions").await,
        Some(serde_json::json!({ "enforced": true })),
        "a new store decides with each person's own permissions from its first day (ADR-0158)"
    );
    // One token for every country, because the store has no country yet when the console gives it
    // these values. Its edge reads the language from its `locale` node once that names one.
    assert_eq!(
        tenant_layer_node(&config_trees, third, "printing").await,
        Some(serde_json::json!({ "receipt_language": "RECEIPT_LANGUAGE_COUNTRY" })),
        "a new store prints its receipts in its country's language (the owner, 2026-10-01)"
    );
    assert_eq!(
        tenant_layer_node(&config_trees, third, "qr").await,
        Some(serde_json::json!({ "table_order": "TABLE_ORDER_JOIN" })),
        "a new store's guest orders join the table's order (the owner, 2026-10-01)"
    );
    assert_eq!(
        tenant_layer_node(&config_trees, third, "counter").await,
        Some(serde_json::json!({ "walk_in_channel": "WALK_IN_CHANNEL_ASK" })),
        "a new store's counter asks each walk-in to eat in or take away (the owner, 2026-10-29)"
    );

    let again = router
        .oneshot(post_with_cookie(
            "/admin/settings/presets",
            &presets,
            &cookie,
        ))
        .await
        .expect("route the presets");
    assert_eq!(
        json_body(again).await["applied"],
        serde_json::json!([]),
        "a store that already sets the value keeps it"
    );
}

#[tokio::test]
async fn a_group_value_reaches_a_store_that_joins_once_it_is_republished() {
    let (router, config_trees, groups) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, second, third] = settings_stores();

    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &put_setting(
                "SETTING_SCOPE_STORE_GROUP",
                &settings_group().to_string(),
                "NO_SHIFT_SELLING_REFUSE",
            ),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(
        outcomes(&json_body(written).await)
            .iter()
            .map(|(store, _)| store.clone())
            .collect::<Vec<_>>(),
        vec![first.to_string(), third.to_string()]
    );
    assert_eq!(tenant_layer_shift(&config_trees, second).await, None);

    groups
        .members
        .lock()
        .expect("lock")
        .insert((tenant(), settings_group()), vec![first, second, third]);
    let republished = router
        .oneshot(post_with_cookie(
            "/admin/settings/publish",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": second.to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(republished.status(), StatusCode::OK);
    assert_eq!(
        tenant_layer_shift(&config_trees, second).await.as_deref(),
        Some("NO_SHIFT_SELLING_REFUSE")
    );
}

#[tokio::test]
async fn a_write_the_register_or_the_tenant_refuses_reaches_no_store() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();
    let store = first.to_string();

    let refused = |body: serde_json::Value| {
        let router = router.clone();
        let cookie = cookie.clone();
        async move {
            router
                .oneshot(put_with_cookie("/admin/settings", &body, &cookie))
                .await
                .expect("route the write")
                .status()
        }
    };
    let unknown_setting = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "setting_key": "shift.no_such_setting",
        "scope": "SETTING_SCOPE_STORE",
        "scope_id": store,
        "value": "NO_SHIFT_SELLING_REFUSE",
    });
    assert_eq!(refused(unknown_setting).await, StatusCode::BAD_REQUEST);
    assert_eq!(
        refused(put_setting(
            "SETTING_SCOPE_STORE",
            &store,
            "NO_SHIFT_SELLING_LATER"
        ))
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        refused(put_setting(
            "SETTING_SCOPE_PLANET",
            &store,
            "NO_SHIFT_SELLING_REFUSE"
        ))
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        refused(put_setting(
            "SETTING_SCOPE_TENANT",
            &Ulid::from_u128(0xBAD).to_string(),
            "NO_SHIFT_SELLING_REFUSE"
        ))
        .await,
        StatusCode::BAD_REQUEST,
        "a tenant-scope value names its own tenant"
    );
    assert_eq!(
        refused(put_setting(
            "SETTING_SCOPE_BRAND",
            &Ulid::from_u128(0xBAD).to_string(),
            "NO_SHIFT_SELLING_REFUSE"
        ))
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        refused(put_setting(
            "SETTING_SCOPE_STORE_GROUP",
            &archived_group().to_string(),
            "NO_SHIFT_SELLING_REFUSE"
        ))
        .await,
        StatusCode::CONFLICT,
        "an archived group's values would reach no store"
    );
    assert_eq!(tenant_layer_shift(&config_trees, first).await, None);
}

#[tokio::test]
async fn the_catalogue_lists_each_setting_with_its_default_and_new_store_value() {
    let (router, _, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let catalogue = router
        .oneshot(get_with_cookie("/admin/settings/catalogue", &cookie))
        .await
        .expect("route the catalogue");
    assert_eq!(catalogue.status(), StatusCode::OK);
    let body = json_body(catalogue).await;
    let shift = body["settings"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|setting| setting["setting_key"] == NO_SHIFT_SELLING)
        .expect("the no-shift setting is listed");
    assert_eq!(shift["default"], "NO_SHIFT_SELLING_ALLOW");
    assert_eq!(shift["preset"], "NO_SHIFT_SELLING_REFUSE");
    assert_eq!(shift["kind"], "SETTING_KIND_CHOICE");
    assert!(
        shift.get("summary").is_none(),
        "the screen labels a setting from its own translations"
    );
    // A choice carries its values and nothing a whole number would: the screen draws its form from
    // which of the two it finds.
    assert_eq!(
        shift["values"],
        serde_json::json!(["NO_SHIFT_SELLING_ALLOW", "NO_SHIFT_SELLING_REFUSE"])
    );
    for absent in ["min", "max", "unit"] {
        assert!(
            shift.get(absent).is_none(),
            "a choice carries no `{absent}`"
        );
    }
}

#[tokio::test]
async fn the_printing_settings_are_offered_as_the_register_says_and_reach_their_node() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();

    // Offered as the screen draws them: a choice of language, and a switch.
    let catalogue = router
        .clone()
        .oneshot(get_with_cookie("/admin/settings/catalogue", &cookie))
        .await
        .expect("route the catalogue");
    let body = json_body(catalogue).await;
    let listed = |key: &str| {
        body["settings"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|setting| setting["setting_key"] == key)
            .cloned()
            .unwrap_or_else(|| panic!("{key} is listed"))
    };
    let language = listed("printing.receipt_language");
    assert_eq!(language["kind"], "SETTING_KIND_CHOICE");
    assert_eq!(language["default"], "RECEIPT_LANGUAGE_DISPLAY");
    assert_eq!(language["preset"], "RECEIPT_LANGUAGE_COUNTRY");
    let on_settle = listed("printing.receipt_printed_on_settle");
    assert_eq!(on_settle["kind"], "SETTING_KIND_BOOL");
    assert_eq!(on_settle["default"], true);
    // Each tax line's components print unless a store turns them off, which every receipt did
    // before the switch (ADR-0168 decision 4), so a new store is given nothing else.
    let components = listed("printing.receipt_tax_components");
    assert_eq!(components["kind"], "SETTING_KIND_BOOL");
    assert_eq!(components["default"], true);
    assert!(components["preset"].is_null(), "{components}");
    // A bilingual receipt is a choice each store makes, so a new store is given none (the owner,
    // 2026-10-01).
    let second = listed("printing.receipt_second_language");
    assert_eq!(second["kind"], "SETTING_KIND_CHOICE");
    assert_eq!(second["default"], "RECEIPT_SECOND_LANGUAGE_NONE");
    assert!(second["preset"].is_null(), "{second}");
    // The size a line the printer cannot draw is drawn at, in printer dots (ADR-0160 decision 6).
    let size = listed("printing.font_size_dots");
    assert_eq!(size["kind"], "SETTING_KIND_INT");
    assert_eq!(size["unit"], "SETTING_UNIT_COUNT");
    assert_eq!(size["min"], 16);
    assert_eq!(size["max"], 48);
    assert_eq!(size["default"], 24);
    assert!(size["preset"].is_null(), "{size}");

    // A store that turns the switches off and sets a size has all three on its `printing` node.
    for (setting_key, value) in [
        (
            "printing.receipt_printed_on_settle",
            serde_json::json!(false),
        ),
        ("printing.font_size_dots", serde_json::json!(32)),
        ("printing.receipt_tax_components", serde_json::json!(false)),
    ] {
        let written = router
            .clone()
            .oneshot(put_with_cookie(
                "/admin/settings",
                &serde_json::json!({
                    "tenant_id": tenant().as_ulid().to_string(),
                    "setting_key": setting_key,
                    "scope": "SETTING_SCOPE_STORE",
                    "scope_id": first.to_string(),
                    "value": value,
                }),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(written.status(), StatusCode::OK, "{setting_key}");
    }
    assert_eq!(
        tenant_layer_node(&config_trees, first, "printing").await,
        Some(serde_json::json!({
            "receipt_printed_on_settle": false,
            "font_size_dots": 32,
            "receipt_tax_components": false,
        }))
    );
}

/// `shift.drawer_model` (ADR-0167 decisions 1 and 7) is offered from the release that ships the
/// till's screens for a drawer per till, with a drawer per till for a new store, and a store's value
/// reaches its `shift` node.
#[tokio::test]
async fn the_drawer_model_is_offered_from_its_release_with_a_drawer_per_till_for_a_new_store() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();

    let catalogue = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/settings/catalogue", &cookie))
            .await
            .expect("route the catalogue"),
    )
    .await;
    let model = catalogue["settings"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|setting| setting["setting_key"] == "shift.drawer_model")
        .cloned()
        .expect("the setting is listed");
    assert_eq!(model["kind"], "SETTING_KIND_CHOICE");
    assert_eq!(
        model["default"], "DRAWER_MODEL_PER_STORE",
        "a store that sets nothing keeps its one drawer"
    );
    assert_eq!(
        model["preset"], "DRAWER_MODEL_PER_TERMINAL",
        "a new store keeps a drawer for each till"
    );
    assert_eq!(
        model["values"],
        serde_json::json!(["DRAWER_MODEL_PER_STORE", "DRAWER_MODEL_PER_TERMINAL"])
    );
    assert_eq!(
        model["since"], "0.14.1",
        "a store on an older release is not offered it, and keeps one drawer"
    );

    let written = router
        .oneshot(put_with_cookie(
            "/admin/settings",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "setting_key": "shift.drawer_model",
                "scope": "SETTING_SCOPE_STORE",
                "scope_id": first.to_string(),
                "value": "DRAWER_MODEL_PER_TERMINAL",
            }),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    assert_eq!(
        tenant_layer_node(&config_trees, first, "shift").await,
        Some(serde_json::json!({ "drawer_model": "DRAWER_MODEL_PER_TERMINAL" }))
    );
}

/// `shift.close_report` (ADR-0167 decision 10) is offered from the release whose edge honours it,
/// and a store's value reaches its `shift` node.
#[tokio::test]
async fn the_close_report_is_offered_from_its_release_and_reaches_the_shift_node() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();

    let catalogue = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/settings/catalogue", &cookie))
            .await
            .expect("route the catalogue"),
    )
    .await;
    let close_report = catalogue["settings"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|setting| setting["setting_key"] == "shift.close_report")
        .cloned()
        .expect("the setting is listed");
    assert_eq!(close_report["kind"], "SETTING_KIND_CHOICE");
    assert_eq!(
        close_report["default"], "CLOSE_REPORT_PER_DRAWER",
        "a store that sets nothing prints each drawer's report where it closes, as before"
    );
    assert!(close_report["preset"].is_null(), "{close_report}");
    assert_eq!(
        close_report["values"],
        serde_json::json!([
            "CLOSE_REPORT_PER_DRAWER",
            "CLOSE_REPORT_COMBINED",
            "CLOSE_REPORT_NONE"
        ])
    );
    assert_eq!(
        close_report["since"], "0.14.1",
        "a store on an older release is not offered it"
    );

    let written = router
        .oneshot(put_with_cookie(
            "/admin/settings",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "setting_key": "shift.close_report",
                "scope": "SETTING_SCOPE_STORE",
                "scope_id": first.to_string(),
                "value": "CLOSE_REPORT_COMBINED",
            }),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    assert_eq!(
        tenant_layer_node(&config_trees, first, "shift").await,
        Some(serde_json::json!({ "close_report": "CLOSE_REPORT_COMBINED" }))
    );
}

/// `shift.drawer_day_end` (ADR-0167 decision 11) is offered from the release whose edge honours
/// it, and a store's value reaches its `shift` node.
#[tokio::test]
async fn the_drawer_day_end_is_offered_from_its_release_and_reaches_the_shift_node() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();

    let catalogue = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/settings/catalogue", &cookie))
            .await
            .expect("route the catalogue"),
    )
    .await;
    let day_end = catalogue["settings"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|setting| setting["setting_key"] == "shift.drawer_day_end")
        .cloned()
        .expect("the setting is listed");
    assert_eq!(day_end["kind"], "SETTING_KIND_CHOICE");
    assert_eq!(
        day_end["default"], "DRAWER_DAY_END_FLAG",
        "a store that sets nothing refuses nothing"
    );
    assert!(day_end["preset"].is_null(), "{day_end}");
    assert_eq!(
        day_end["values"],
        serde_json::json!(["DRAWER_DAY_END_FLAG", "DRAWER_DAY_END_REQUIRE_CLOSE"])
    );
    assert_eq!(
        day_end["since"], "0.14.1",
        "a store on an older release is not offered it"
    );

    let written = router
        .oneshot(put_with_cookie(
            "/admin/settings",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "setting_key": "shift.drawer_day_end",
                "scope": "SETTING_SCOPE_STORE",
                "scope_id": first.to_string(),
                "value": "DRAWER_DAY_END_REQUIRE_CLOSE",
            }),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    assert_eq!(
        tenant_layer_node(&config_trees, first, "shift").await,
        Some(serde_json::json!({ "drawer_day_end": "DRAWER_DAY_END_REQUIRE_CLOSE" }))
    );
}

/// `qr.table_order` (ADR-0160 item 2) is a setting on a node the console's QR guardrails also
/// publish. Written on the Tenant layer, it reaches the store beside the guardrails the Store layer
/// holds, and leaves them as they are.
#[tokio::test]
async fn the_qr_table_order_reaches_the_qr_node_beside_its_guardrails() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();

    let catalogue = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/settings/catalogue", &cookie))
            .await
            .expect("route the catalogue"),
    )
    .await;
    let table_order = catalogue["settings"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|setting| setting["setting_key"] == "qr.table_order")
        .cloned()
        .expect("the setting is listed");
    assert_eq!(table_order["kind"], "SETTING_KIND_CHOICE");
    assert_eq!(
        table_order["default"], "TABLE_ORDER_SEPARATE",
        "a store that sets nothing keeps a guest's order separate, as before"
    );
    assert_eq!(table_order["preset"], "TABLE_ORDER_JOIN");
    assert_eq!(
        table_order["values"],
        serde_json::json!(["TABLE_ORDER_SEPARATE", "TABLE_ORDER_JOIN"])
    );
    assert_eq!(table_order["since"], "0.14.1");

    let guardrails = serde_json::json!({
        "enabled": true,
        "staff_confirmation_required": true,
        "per_table_limit": 5,
    });
    config_trees.seed_store_layer(
        tenant(),
        first,
        serde_json::json!({ "qr": guardrails.clone() }),
    );
    let written = router
        .oneshot(put_with_cookie(
            "/admin/settings",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "setting_key": "qr.table_order",
                "scope": "SETTING_SCOPE_STORE",
                "scope_id": first.to_string(),
                "value": "TABLE_ORDER_JOIN",
            }),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);

    let tree = config_trees
        .load(tenant(), first)
        .await
        .expect("load")
        .expect("a tree");
    assert_eq!(
        tree.record.layers[0]["qr"],
        serde_json::json!({ "table_order": "TABLE_ORDER_JOIN" })
    );
    assert_eq!(
        tree.record.layers[2]["qr"], guardrails,
        "the guardrails untouched"
    );
    assert_eq!(
        sent_to(&tree.record)["qr"],
        serde_json::json!({
            "enabled": true,
            "staff_confirmation_required": true,
            "per_table_limit": 5,
            "table_order": "TABLE_ORDER_JOIN",
        }),
        "the store is sent one `qr` node with both"
    );
}

#[tokio::test]
async fn a_value_of_the_wrong_kind_is_refused_with_the_reason_the_register_gives() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();

    // A number and a boolean are values of other kinds of setting, not tokens of this choice: the
    // register's one rule refuses them at the write, before anything reaches a store.
    for value in [serde_json::json!(1), serde_json::json!(true)] {
        let refused = router
            .clone()
            .oneshot(put_with_cookie(
                "/admin/settings",
                &serde_json::json!({
                    "tenant_id": tenant().as_ulid().to_string(),
                    "setting_key": NO_SHIFT_SELLING,
                    "scope": "SETTING_SCOPE_STORE",
                    "scope_id": first.to_string(),
                    "value": value,
                }),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{value}");
        let body = json_body(refused).await;
        assert_eq!(body["error"]["details"][0]["field"], "value");
        assert_eq!(body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE");
    }
    assert_eq!(tenant_layer_shift(&config_trees, first).await, None);
}
