// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Settings that reach a store as numbers or on nodes of their own: the session, the archive
//! interval, the permissions switch and the tip keys (ADR-0160 decision 3).

use axum::http::StatusCode;
use pos_cloud::config_tree::ConfigTreeStore;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    LOCKOUT_ATTEMPTS, admin_cookie, get_with_cookie, json_body, outcomes, put_number,
    put_with_cookie, sent_to, settings_app, settings_stores, tenant, tenant_layer_node,
    tenant_layer_session,
};

#[tokio::test]
async fn a_whole_number_setting_is_published_to_every_store_as_a_number() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;

    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &put_number(LOCKOUT_ATTEMPTS, &serde_json::json!(3)),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    assert!(
        outcomes(&json_body(written).await)
            .iter()
            .all(|(_, outcome)| outcome == "SETTING_PUBLISH_APPLIED"),
        "every store of the tenant is given it"
    );
    for store_id in settings_stores() {
        assert_eq!(
            tenant_layer_session(&config_trees, store_id).await,
            Some(serde_json::json!({ "lockout_attempts": 3 })),
            "a number, as the edge's `session` node reads it"
        );
    }

    // What one store runs, with its default beside it for the settings nothing sets.
    let [first, _, _] = settings_stores();
    let runs = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!(
                    "/admin/settings/effective?tenant_id={}&store_id={first}",
                    tenant().as_ulid()
                ),
                &cookie,
            ))
            .await
            .expect("route"),
    )
    .await;
    let running = |key: &str| {
        runs["settings"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|setting| setting["setting_key"] == key)
            .cloned()
            .unwrap_or_else(|| panic!("{key} is listed"))
    };
    assert_eq!(running(LOCKOUT_ATTEMPTS)["value"], 3);
    assert_eq!(running(LOCKOUT_ATTEMPTS)["scope"], "SETTING_SCOPE_TENANT");
    assert_eq!(running("session.lockout_minutes")["value"], 5);
    assert!(running("session.lockout_minutes").get("scope").is_none());

    // The catalogue gives the console the bounds and the unit to draw its field from.
    let catalogue = json_body(
        router
            .oneshot(get_with_cookie("/admin/settings/catalogue", &cookie))
            .await
            .expect("route the catalogue"),
    )
    .await;
    let attempts = catalogue["settings"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|setting| setting["setting_key"] == LOCKOUT_ATTEMPTS)
        .expect("the lockout attempts are listed");
    assert_eq!(attempts["kind"], "SETTING_KIND_INT");
    assert_eq!(attempts["min"], 3);
    assert_eq!(attempts["max"], 10);
    assert_eq!(attempts["unit"], "SETTING_UNIT_COUNT");
    assert_eq!(attempts["default"], 5);
    assert!(attempts.get("preset").is_none(), "a new store keeps five");
    assert!(attempts.get("values").is_none(), "a number has no list");
}

/// The archive interval (ADR-0160 decision 6) is written on a node of its own, `backup`, on each
/// store's Tenant layer, as a number in hours; the console is told its unit and that it cannot be
/// written as zero.
#[tokio::test]
async fn the_archive_interval_reaches_each_store_on_a_backup_node_of_its_own() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;

    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &put_number("backup.interval_hours", &serde_json::json!(6)),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    for store_id in settings_stores() {
        assert_eq!(
            tenant_layer_node(&config_trees, store_id, "backup").await,
            Some(serde_json::json!({ "interval_hours": 6 })),
            "a number, on the node the edge reads it from"
        );
    }

    // No published value switches archiving off.
    let refused = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &put_number("backup.interval_hours", &serde_json::json!(0)),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(refused).await["error"]["details"][0]["reason"],
        "OUT_OF_RANGE"
    );

    let catalogue = json_body(
        router
            .oneshot(get_with_cookie("/admin/settings/catalogue", &cookie))
            .await
            .expect("route the catalogue"),
    )
    .await;
    let interval = catalogue["settings"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|setting| setting["setting_key"] == "backup.interval_hours")
        .expect("the archive interval is listed");
    assert_eq!(interval["unit"], "SETTING_UNIT_HOURS");
    assert_eq!(interval["min"], 1);
    assert_eq!(interval["max"], 168);
    assert_eq!(interval["default"], 24);
}

/// The enforcement switch (ADR-0158 Rollout) is written on each store's Tenant layer, and the
/// merge puts it beside the staff the people compiler wrote on the Store layer. A store with no
/// people node receives the switch alone, with no `staff` key, which the edge reads as the switch
/// and nothing about its roster.
#[tokio::test]
async fn the_permissions_switch_reaches_each_store_beside_its_people() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, second, _] = settings_stores();
    let people = serde_json::json!({
        "store_id": first.to_string(),
        "staff": [{
            "id": Ulid::from_u128(0xE1).to_string(),
            "code": "C01",
            "name": "Staff one",
            "permissions": ["sales.line.add"],
            "pin_phc": null
        }]
    });
    config_trees.seed_store_layer(
        tenant(),
        first,
        serde_json::json!({ "permissions": people.clone() }),
    );

    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "setting_key": "permissions.enforced",
                "scope": "SETTING_SCOPE_TENANT",
                "scope_id": tenant().as_ulid().to_string(),
                "value": true,
            }),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);
    assert!(
        outcomes(&json_body(written).await)
            .iter()
            .all(|(_, outcome)| outcome == "SETTING_PUBLISH_APPLIED")
    );

    // The staffed store: the switch on the Tenant layer, the people untouched on the Store layer,
    // and the two together in what the store is sent.
    let staffed = config_trees
        .load(tenant(), first)
        .await
        .expect("load")
        .expect("a tree");
    assert_eq!(
        staffed.record.layers[0]["permissions"],
        serde_json::json!({ "enforced": true })
    );
    assert_eq!(staffed.record.layers[2]["permissions"], people);
    let sent = sent_to(&staffed.record);
    assert_eq!(sent["permissions"]["enforced"], true);
    assert_eq!(sent["permissions"]["staff"], people["staff"]);

    // A store with no people node is sent the switch alone. With no `staff` key the edge keeps
    // the roster it holds rather than reading the store as staffed by nobody.
    let unstaffed = config_trees
        .load(tenant(), second)
        .await
        .expect("load")
        .expect("a tree");
    assert_eq!(
        sent_to(&unstaffed.record)["permissions"],
        serde_json::json!({ "enforced": true })
    );
}

#[tokio::test]
async fn a_number_outside_its_bounds_or_not_whole_is_refused_and_reaches_no_store() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;

    for (setting_key, value, reason) in [
        // No value switches the PIN lockout off: zero attempts and zero minutes are out of range.
        (LOCKOUT_ATTEMPTS, serde_json::json!(0), "OUT_OF_RANGE"),
        (LOCKOUT_ATTEMPTS, serde_json::json!(11), "OUT_OF_RANGE"),
        (
            "session.lockout_minutes",
            serde_json::json!(0),
            "OUT_OF_RANGE",
        ),
        (
            "session.sign_in_idle_timeout_minutes",
            serde_json::json!(241),
            "OUT_OF_RANGE",
        ),
        (
            "session.sign_in_idle_timeout_minutes",
            serde_json::json!(4),
            "OUT_OF_RANGE",
        ),
        // A value of another JSON type is refused, not coerced.
        (LOCKOUT_ATTEMPTS, serde_json::json!(3.5), "INVALID_VALUE"),
        (LOCKOUT_ATTEMPTS, serde_json::json!("3"), "INVALID_VALUE"),
        (LOCKOUT_ATTEMPTS, serde_json::json!(true), "INVALID_VALUE"),
    ] {
        let refused = router
            .clone()
            .oneshot(put_with_cookie(
                "/admin/settings",
                &put_number(setting_key, &value),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "{setting_key} = {value}"
        );
        let body = json_body(refused).await;
        assert_eq!(body["error"]["details"][0]["field"], "value");
        assert_eq!(
            body["error"]["details"][0]["reason"], reason,
            "{setting_key} = {value}"
        );
    }
    for store_id in settings_stores() {
        assert_eq!(tenant_layer_session(&config_trees, store_id).await, None);
    }
}

/// The tip keys are settings on a `tender_keys` node of their own (ADR-0160 decision 2), written on
/// each store's Tenant layer. The `tender` node the tender publish writes on the Store layer is left
/// as it was, and a store whose tender was never published receives the keys and no `tender` node.
#[tokio::test]
async fn the_tip_keys_reach_a_store_on_their_own_node_and_leave_its_tender_alone() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, third] = settings_stores();
    let accepted =
        serde_json::json!({ "accepted": ["PAYMENT_METHOD_CASH", "PAYMENT_METHOD_CARD"] });
    let effective = |store_id: StoreId| {
        get_with_cookie(
            &format!(
                "/admin/stores/{}/config?tenant_id={}",
                store_id.as_ulid(),
                tenant().as_ulid()
            ),
            &cookie,
        )
    };

    let published = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/tender",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": first.to_string(),
                "accepted": accepted["accepted"],
            }),
            &cookie,
        ))
        .await
        .expect("route the tender publish");
    assert_eq!(published.status(), StatusCode::OK);
    for (key, value) in [
        ("tender_keys.first_tip_percent", 10),
        ("tender_keys.second_tip_percent", 0),
    ] {
        let written = router
            .clone()
            .oneshot(put_with_cookie(
                "/admin/settings",
                &serde_json::json!({
                    "tenant_id": tenant().as_ulid().to_string(),
                    "setting_key": key,
                    "scope": "SETTING_SCOPE_TENANT",
                    "scope_id": tenant().as_ulid().to_string(),
                    "value": value,
                }),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(written.status(), StatusCode::OK, "{key}");
    }
    let keys = serde_json::json!({ "first_tip_percent": 10, "second_tip_percent": 0 });
    let tree = config_trees
        .load(tenant(), first)
        .await
        .expect("load")
        .expect("a tree");
    assert_eq!(
        tree.record.layers[0]["tender_keys"], keys,
        "on the Tenant layer"
    );
    assert!(tree.record.layers[0].get("tender").is_none());
    assert_eq!(
        tree.record.layers[2]["tender"], accepted,
        "the store's tender as published"
    );

    let listed = json_body(
        router
            .clone()
            .oneshot(effective(first))
            .await
            .expect("route"),
    )
    .await;
    assert_eq!(listed["tender"], accepted);
    assert_eq!(listed["tender_keys"], keys);
    let unlisted = json_body(router.oneshot(effective(third)).await.expect("route")).await;
    assert_eq!(unlisted["tender_keys"], keys);
    assert!(
        unlisted.get("tender").is_none(),
        "no tender node, so the store takes every tender as before"
    );
}
